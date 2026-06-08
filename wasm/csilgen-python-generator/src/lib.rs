//! Python code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target python` from `csilgen_python_generator.wasm`.

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    CsilgenError, GeneratedFile, GeneratedFiles, GenerationStats, GeneratorCapability,
    GeneratorConfig, GeneratorMetadata, GeneratorWarning, Result, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// WASM exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "python-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Python code generator".to_string(),
        target: "python".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: None,
    };
    write_json_to_wasm(&metadata) as *const u8
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
    match process_generation(input_ptr, input_len) {
        Ok(output) => write_json_to_wasm(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn write_json_to_wasm<T: serde::Serialize>(value: &T) -> *mut u8 {
    let json = match serde_json::to_string(value) {
        Ok(j) => j,
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

fn process_generation(
    input_ptr: *const u8,
    input_len: usize,
) -> std::result::Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let bytes = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let s = std::str::from_utf8(bytes).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(s).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let files = generate_python_code_from_serialized(&input.csil_spec, &input.config)
        .map_err(|_| error_codes::GENERATION_ERROR)?;

    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: files.iter().map(|f| f.content.len()).sum(),
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 0,
        peak_memory_bytes: None,
    };
    Ok(WasmGeneratorOutput {
        files,
        warnings: Vec::<GeneratorWarning>::new(),
        stats,
    })
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

fn csil_literal_to_python_str(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Text(text) => format!("\"{text}\""),
        CsilLiteralValue::Integer(num) => num.to_string(),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Null => "None".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            format!("b\"{}\"", String::from_utf8_lossy(bytes))
        }
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(csil_literal_to_python_str).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

/// Generate Python dataclasses from serialized CDDL specification
pub fn generate_python_code_from_serialized(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<GeneratedFiles> {
    let mut generator = PythonGenerator::new(config);
    generator.generate(spec)
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — the error half is
/// raised by the transport, not part of the returned value.
fn python_success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !matches!(c, CsilTypeExpression::Reference(name) if name == "ServiceError"))
            .cloned()
            .collect();
        match kept.len() {
            1 => kept.into_iter().next().unwrap(),
            0 => type_expr.clone(),
            _ => CsilTypeExpression::Choice(kept),
        }
    } else {
        type_expr.clone()
    }
}

/// PascalCase an operation name for the wire, using the same simple rule the
/// TypeScript/Go/Rust clients use so all four agree on the method string.
fn wire_method_name(name: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            cap = true;
        } else if cap {
            out.extend(ch.to_uppercase());
            cap = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Python code generator implementation
struct PythonGenerator {
    #[allow(dead_code)]
    config: GeneratorConfig,
    use_pydantic: bool,
    generated_types: HashSet<String>,
    imports: HashSet<String>,
}

impl PythonGenerator {
    fn new(config: &GeneratorConfig) -> Self {
        let use_pydantic = config
            .options
            .get("use_pydantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Self {
            config: config.clone(),
            use_pydantic,
            generated_types: HashSet::new(),
            imports: HashSet::new(),
        }
    }

    fn generate(&mut self, spec: &CsilSpecSerialized) -> Result<GeneratedFiles> {
        // Dispatch on target: the base `python` (and explicit `python-server`)
        // target emits server-side handler ABCs; `python-client` emits
        // transport-agnostic clients; `python-typesonly` emits the dataclasses
        // alone. An unrecognized sub-target is an error, not a silent fall-through.
        enum Surface {
            Server,
            Client,
            TypesOnly,
        }
        let surface = match self.config.target.as_str() {
            "python" | "python-server" => Surface::Server,
            "python-client" => Surface::Client,
            "python-typesonly" => Surface::TypesOnly,
            other => {
                return Err(CsilgenError::GenerationError(format!(
                    "Unknown python sub-target '{other}'. Supported: python, python-server, python-client, python-typesonly"
                )));
            }
        };

        let mut files = Vec::new();

        self.setup_imports();

        let mut types_code = String::new();
        let mut services_code = String::new();

        // Detect channel ops once so the services prelude (Codec) is emitted
        // exactly once at the top of the services file, not per-service.
        let has_channel_ops = spec.rules.iter().any(|r| {
            matches!(&r.rule_type, CsilRuleType::ServiceDef(def)
                if Self::service_has_channel_ops(def))
        });

        let mut prelude_emitted = false;

        for rule in &spec.rules {
            match &rule.rule_type {
                CsilRuleType::TypeDef(type_expr) => {
                    types_code.push_str(&self.generate_type_def(&rule.name, type_expr)?);
                }
                CsilRuleType::GroupDef(group_expr) => {
                    types_code.push_str(&self.generate_group_def(&rule.name, group_expr)?);
                }
                CsilRuleType::TypeChoice(choices) => {
                    types_code.push_str(&self.generate_type_choice(&rule.name, choices)?);
                }
                CsilRuleType::GroupChoice(choices) => {
                    types_code.push_str(&self.generate_group_choice(&rule.name, choices)?);
                }
                CsilRuleType::ServiceDef(service) => match &surface {
                    Surface::TypesOnly => {}
                    Surface::Client => {
                        if !prelude_emitted {
                            services_code.push_str(&Self::generate_client_prelude());
                            prelude_emitted = true;
                        }
                        services_code.push_str(&self.generate_client_class(&rule.name, service)?);
                    }
                    Surface::Server => {
                        if !prelude_emitted {
                            services_code
                                .push_str(&Self::generate_services_prelude(has_channel_ops));
                            prelude_emitted = true;
                        }
                        services_code
                            .push_str(&self.generate_service_artifacts(&rule.name, service)?);
                    }
                },
            }
        }

        if !types_code.is_empty() {
            let types_file = self.generate_types_file(types_code)?;
            files.push(types_file);
        }

        if !services_code.is_empty() {
            let module_file =
                self.generate_module_file(services_code, matches!(surface, Surface::Client))?;
            files.push(module_file);
        }

        if !files.is_empty() {
            let init_file = self.generate_init_file(&files)?;
            files.push(init_file);
        }

        Ok(files)
    }

    fn setup_imports(&mut self) {
        self.imports
            .insert("from typing import Optional, List, Dict, Any, Union".to_string());
        self.imports.insert("import json".to_string());

        if self.use_pydantic {
            self.imports
                .insert("from pydantic import BaseModel, Field, validator".to_string());
        } else {
            self.imports
                .insert("from dataclasses import dataclass, field".to_string());
        }
    }

    fn generate_type_def(&mut self, name: &str, type_expr: &CsilTypeExpression) -> Result<String> {
        // `Name = { ... }` parses to a TypeDef carrying a Group expression. Emit a
        // real dataclass for it (as the Rust/Go generators do) instead of a bare
        // `Dict[str, Any]` alias, so records keep field-level typing. Named scalar
        // and map aliases stay aliases via the fallthrough below.
        if let CsilTypeExpression::Group(group) = type_expr {
            return self.generate_group_def(name, group);
        }

        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        let python_type = self.map_type_expression(type_expr)?;

        Ok(format!("{class_name} = {python_type}\n\n"))
    }

    fn generate_group_def(&mut self, name: &str, group: &CsilGroupExpression) -> Result<String> {
        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        let mut code = String::new();

        if self.use_pydantic {
            code.push_str(&format!("class {class_name}(BaseModel):\n"));
        } else {
            code.push_str("@dataclass\n");
            code.push_str(&format!("class {class_name}:\n"));
        }

        if group.entries.is_empty() {
            code.push_str("    pass\n");
        } else {
            for entry in &group.entries {
                code.push_str(&self.generate_field(entry)?);
            }

            if !self.use_pydantic {
                code.push_str(&self.generate_serialization_methods(&class_name, &group.entries)?);
                code.push_str(&self.generate_validation_methods(&class_name, &group.entries)?);
            } else {
                code.push_str(&self.generate_pydantic_validators(&class_name, &group.entries)?);
            }
        }

        code.push('\n');
        Ok(code)
    }

    fn generate_field(&self, entry: &CsilGroupEntry) -> Result<String> {
        let field_name = match &entry.key {
            Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => name.to_case(Case::Snake),
            _ => "field".to_string(),
        };

        let python_type = self.map_type_expression(&entry.value_type)?;
        let is_optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));

        let field_type = if is_optional {
            format!("Optional[{python_type}]")
        } else {
            python_type
        };

        let mut field_definition = String::new();

        if let Some(description) = self.get_field_description(&entry.metadata) {
            field_definition.push_str(&format!("    # {description}\n"));
        }

        if self.use_pydantic {
            let field_config = self.generate_pydantic_field_config(entry)?;
            if field_config.is_empty() {
                field_definition.push_str(&format!("    {field_name}: {field_type}\n"));
            } else {
                field_definition.push_str(&format!(
                    "    {field_name}: {field_type} = Field({field_config})\n"
                ));
            }
        } else {
            let default_value = if is_optional { " = None" } else { "" };
            field_definition.push_str(&format!("    {field_name}: {field_type}{default_value}\n"));
        }

        Ok(field_definition)
    }

    fn generate_pydantic_field_config(&self, entry: &CsilGroupEntry) -> Result<String> {
        let mut config_parts = Vec::new();

        if let Some(description) = self.get_field_description(&entry.metadata) {
            config_parts.push(format!(
                "description=\"{}\"",
                description.replace('"', "\\\"")
            ));
        }

        for metadata in &entry.metadata {
            match metadata {
                CsilFieldMetadata::Constraint(constraint) => match constraint {
                    CsilValidationConstraint::MinLength(min) => {
                        config_parts.push(format!("min_length={min}"));
                    }
                    CsilValidationConstraint::MaxLength(max) => {
                        config_parts.push(format!("max_length={max}"));
                    }
                    CsilValidationConstraint::MinItems(min) => {
                        config_parts.push(format!("min_items={min}"));
                    }
                    CsilValidationConstraint::MaxItems(max) => {
                        config_parts.push(format!("max_items={max}"));
                    }
                    _ => {}
                },
                CsilFieldMetadata::Custom { name, parameters } if name == "pydantic" => {
                    for param in parameters {
                        if let Some(param_name) = &param.name {
                            match &param.value {
                                CsilLiteralValue::Text(value) => {
                                    config_parts.push(format!("{param_name}=\"{value}\""));
                                }
                                CsilLiteralValue::Bool(value) => {
                                    config_parts.push(format!("{param_name}={value}"));
                                }
                                CsilLiteralValue::Integer(value) => {
                                    config_parts.push(format!("{param_name}={value}"));
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(config_parts.join(", "))
    }

    fn get_field_description(&self, metadata: &[CsilFieldMetadata]) -> Option<String> {
        metadata.iter().find_map(|m| match m {
            CsilFieldMetadata::Description(desc) => Some(desc.clone()),
            _ => None,
        })
    }

    fn generate_serialization_methods(
        &self,
        class_name: &str,
        entries: &[CsilGroupEntry],
    ) -> Result<String> {
        let mut code = String::new();

        code.push_str("    def to_dict(self) -> Dict[str, Any]:\n");
        code.push_str("        \"\"\"Convert to dictionary for JSON serialization.\"\"\"\n");
        code.push_str("        result = {}\n");

        for entry in entries {
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                    name.to_case(Case::Snake)
                }
                _ => continue,
            };

            let visibility = self.get_field_visibility(&entry.metadata);

            match visibility {
                Some(CsilFieldVisibility::ReceiveOnly) => {
                    continue;
                }
                _ => {
                    code.push_str(&format!("        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"));
                    code.push_str(&format!(
                        "            result['{field_name}'] = self.{field_name}\n"
                    ));
                }
            }
        }

        code.push_str("        return result\n\n");

        code.push_str("    @classmethod\n");
        code.push_str(&format!(
            "    def from_dict(cls, data: Dict[str, Any]) -> '{class_name}':\n"
        ));
        code.push_str("        \"\"\"Create instance from dictionary.\"\"\"\n");

        let mut field_assignments = Vec::new();
        for entry in entries {
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                    name.to_case(Case::Snake)
                }
                _ => continue,
            };

            let visibility = self.get_field_visibility(&entry.metadata);

            match visibility {
                Some(CsilFieldVisibility::SendOnly) => {
                    continue;
                }
                _ => {
                    field_assignments.push(format!("{field_name}=data.get('{field_name}')"));
                }
            }
        }

        code.push_str(&format!(
            "        return cls({})\n\n",
            field_assignments.join(", ")
        ));

        code.push_str("    def to_json(self) -> str:\n");
        code.push_str("        \"\"\"Convert to JSON string.\"\"\"\n");
        code.push_str("        return json.dumps(self.to_dict())\n\n");

        code.push_str("    @classmethod\n");
        code.push_str(&format!(
            "    def from_json(cls, json_str: str) -> '{class_name}':\n"
        ));
        code.push_str("        \"\"\"Create instance from JSON string.\"\"\"\n");
        code.push_str("        return cls.from_dict(json.loads(json_str))\n\n");

        Ok(code)
    }

    fn generate_validation_methods(
        &self,
        _class_name: &str,
        entries: &[CsilGroupEntry],
    ) -> Result<String> {
        let mut code = String::new();

        let dependencies: Vec<_> = entries
            .iter()
            .filter_map(|entry| {
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::DependsOn { field, value } = metadata {
                        let field_name = match &entry.key {
                            Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                                name.to_case(Case::Snake)
                            }
                            _ => continue,
                        };
                        return Some((field_name, field.clone(), value.clone()));
                    }
                }
                None
            })
            .collect();

        if !dependencies.is_empty() {
            code.push_str("    def validate(self) -> bool:\n");
            code.push_str("        \"\"\"Validate field dependencies and constraints.\"\"\"\n");

            for (field_name, depends_on_field, depends_on_value) in &dependencies {
                let dep_field_name = depends_on_field.to_case(Case::Snake);

                match depends_on_value {
                    Some(value) => {
                        let value_str = csil_literal_to_python_str(value);

                        code.push_str(&format!(
                            "        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"
                        ));
                        code.push_str(&format!(
                            "            if not (hasattr(self, '{dep_field_name}') and self.{dep_field_name} == {value_str}):\n"
                        ));
                        code.push_str(&format!(
                            "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be {value_str}\")\n"
                        ));
                    }
                    None => {
                        code.push_str(&format!(
                            "        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"
                        ));
                        code.push_str(&format!(
                            "            if not (hasattr(self, '{dep_field_name}') and self.{dep_field_name} is not None):\n"
                        ));
                        code.push_str(&format!(
                            "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be present\")\n"
                        ));
                    }
                }
            }

            code.push_str("        return True\n\n");

            code.push_str("    def __post_init__(self):\n");
            code.push_str("        \"\"\"Validate object after initialization.\"\"\"\n");
            code.push_str("        self.validate()\n\n");
        }

        Ok(code)
    }

    fn generate_pydantic_validators(
        &self,
        _class_name: &str,
        entries: &[CsilGroupEntry],
    ) -> Result<String> {
        let mut code = String::new();

        let dependencies: Vec<_> = entries
            .iter()
            .filter_map(|entry| {
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::DependsOn { field, value } = metadata {
                        let field_name = match &entry.key {
                            Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                                name.to_case(Case::Snake)
                            }
                            _ => continue,
                        };
                        return Some((field_name, field.clone(), value.clone()));
                    }
                }
                None
            })
            .collect();

        for (field_name, depends_on_field, depends_on_value) in &dependencies {
            let dep_field_name = depends_on_field.to_case(Case::Snake);

            code.push_str(&format!("    @validator('{field_name}')\n"));
            code.push_str(&format!("    def validate_{field_name}(cls, v, values):\n"));
            code.push_str(&format!(
                "        \"\"\"Validate {field_name} field dependencies.\"\"\"\n"
            ));

            match depends_on_value {
                Some(value) => {
                    let value_str = csil_literal_to_python_str(value);

                    code.push_str("        if v is not None:\n");
                    code.push_str(&format!(
                        "            if '{dep_field_name}' not in values or values['{dep_field_name}'] != {value_str}:\n"
                    ));
                    code.push_str(&format!(
                        "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be {value_str}\")\n"
                    ));
                }
                None => {
                    code.push_str("        if v is not None:\n");
                    code.push_str(&format!(
                        "            if '{dep_field_name}' not in values or values['{dep_field_name}'] is None:\n"
                    ));
                    code.push_str(&format!(
                        "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be present\")\n"
                    ));
                }
            }

            code.push_str("        return v\n\n");
        }

        Ok(code)
    }

    fn get_field_visibility(&self, metadata: &[CsilFieldMetadata]) -> Option<CsilFieldVisibility> {
        metadata.iter().find_map(|m| match m {
            CsilFieldMetadata::Visibility(vis) => Some(vis.clone()),
            _ => None,
        })
    }

    fn generate_type_choice(
        &mut self,
        name: &str,
        choices: &[CsilTypeExpression],
    ) -> Result<String> {
        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        let choice_types: Result<Vec<String>> = choices
            .iter()
            .map(|choice| self.map_type_expression(choice))
            .collect();
        let choice_types = choice_types?;

        Ok(format!(
            "{} = Union[{}]\n\n",
            class_name,
            choice_types.join(", ")
        ))
    }

    fn generate_group_choice(
        &mut self,
        name: &str,
        choices: &[CsilGroupExpression],
    ) -> Result<String> {
        let mut code = String::new();

        for (i, choice) in choices.iter().enumerate() {
            let choice_name = format!("{name}Choice{}", i + 1);
            code.push_str(&self.generate_group_def(&choice_name, choice)?);
        }

        let choice_names: Vec<String> = (0..choices.len())
            .map(|i| format!("{name}Choice{}", i + 1))
            .collect();

        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        code.push_str(&format!(
            "{} = Union[{}]\n\n",
            class_name,
            choice_names.join(", ")
        ));

        Ok(code)
    }

    fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
        def.operations
            .iter()
            .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
    }

    /// Once-per-file preamble for the services module: `ServiceError`
    /// exception, plus a `Codec` Protocol when any service has channel ops.
    /// Imports needed for these definitions live inline so the file's existing
    /// imports block (assembled from `self.imports`) isn't affected.
    fn generate_services_prelude(has_channel_ops: bool) -> String {
        let mut out = String::new();
        out.push_str("from abc import ABC, abstractmethod\n");
        if has_channel_ops {
            out.push_str("from typing import Protocol, Any, Tuple\n");
        }
        out.push('\n');
        out.push_str("class ServiceError(Exception):\n");
        out.push_str(
            "    \"\"\"Transport-level error thrown by service routers and handlers.\"\"\"\n",
        );
        out.push_str("    def __init__(self, code: int, message: str):\n");
        out.push_str("        self.code = code\n");
        out.push_str("        self.message = message\n");
        out.push_str("        super().__init__(f\"service error {code}: {message}\")\n\n");

        if has_channel_ops {
            out.push_str("class Codec(Protocol):\n");
            out.push_str(
                "    \"\"\"User-supplied (de)serialization for channel messages.\n\n\
                 \x20   The generator is codec-agnostic; the implementer wires this to CBOR,\n\
                 \x20   JSON, or anything else its protocol expects.\n\
                 \x20   \"\"\"\n",
            );
            out.push_str("    def encode(self, value: Any) -> bytes: ...\n");
            out.push_str("    def decode(self, data: bytes, target_type: type) -> Any: ...\n\n");
        }
        out
    }

    /// Once-per-file preamble for the client module: the `ServiceError`
    /// exception the transport raises, and the `Transport` Protocol every client
    /// delegates to. The generator never owns the wire (CBOR-over-HTTP etc.).
    fn generate_client_prelude() -> String {
        let mut out = String::new();
        out.push_str("from typing import Protocol, Any\n\n");
        out.push_str("class ServiceError(Exception):\n");
        out.push_str(
            "    \"\"\"Structured error a service returns; raised by the transport.\"\"\"\n",
        );
        out.push_str("    def __init__(self, code: int, message: str):\n");
        out.push_str("        self.code = code\n");
        out.push_str("        self.message = message\n");
        out.push_str("        super().__init__(f\"service error {code}: {message}\")\n\n");
        out.push_str("class Transport(Protocol):\n");
        out.push_str(
            "    \"\"\"Caller-supplied wire. Encodes req (CBOR over HTTP, say), performs the\n\
             \x20   call named by (service, method), and returns the decoded response, or\n\
             \x20   raises ServiceError. The generator never owns the wire.\n\
             \x20   \"\"\"\n",
        );
        out.push_str("    def call(self, service: str, method: str, req: Any) -> Any: ...\n\n");
        out
    }

    /// Emit a typed client class for one service: one method per unary operation
    /// that delegates to the `Transport`, returning the typed success response.
    fn generate_client_class(&self, name: &str, service: &CsilServiceDefinition) -> Result<String> {
        let service_class = name.to_case(Case::Pascal);
        let base = service_class
            .strip_suffix("Service")
            .filter(|s| !s.is_empty())
            .unwrap_or(&service_class);
        let client_class = format!("{base}Client");
        let wire_service = base.to_lowercase();

        let mut out = String::new();
        out.push_str(&format!("class {client_class}:\n"));
        out.push_str(&format!(
            "    \"\"\"Typed client for the {name} service.\"\"\"\n"
        ));
        out.push_str("    def __init__(self, transport: Transport):\n");
        out.push_str("        self._transport = transport\n");

        for op in &service.operations {
            // Only unary request/response ops belong on the RPC client; channel
            // ops ride the router/encoder surface emitted by the base target.
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                out.push_str(&format!(
                    "\n    # channel operation {} is not part of the RPC client\n",
                    op.name
                ));
                continue;
            }
            let method_name = op.name.to_case(Case::Snake);
            // The wire method must agree byte-for-byte with the other language
            // clients, which all PascalCase the op name with the same simple
            // rule — convert_case would diverge on acronyms, so avoid it here.
            let wire_method = wire_method_name(&op.name);
            let input_type = self.map_type_expression(&op.input_type)?;
            let output_type = self.map_type_expression(&python_success_type(&op.output_type))?;
            out.push('\n');
            out.push_str(&format!(
                "    def {method_name}(self, req: {input_type}) -> {output_type}:\n"
            ));
            if op.doc_comments.is_empty() {
                out.push_str(&format!("        \"\"\"{}\"\"\"\n", op.name));
            } else {
                out.push_str("        \"\"\"");
                for (i, line) in op.doc_comments.iter().enumerate() {
                    if i > 0 {
                        out.push_str("\n        ");
                    }
                    out.push_str(line);
                }
                out.push_str("\"\"\"\n");
            }
            out.push_str(&format!(
                "        return self._transport.call(\"{wire_service}\", \"{wire_method}\", req)\n"
            ));
        }
        out.push('\n');
        Ok(out)
    }

    /// Emit the server-side handler ABC plus, when channel ops exist, a
    /// `route_<service>_channel` dispatcher and per-op outbound encoders.
    /// Reverse ops contribute only the outbound encoder (server pushes only).
    fn generate_service_artifacts(
        &self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String> {
        let service_class = name.to_case(Case::Pascal);
        let handler_class = format!("{service_class}Handlers");
        let mut out = String::new();

        // Server-side handlers ABC: unidirectional ops return Output; <->
        // inbound is fire-and-forget. Reverse has no server inbound here.
        out.push_str(&format!("class {handler_class}(ABC):\n"));
        out.push_str(&format!(
            "    \"\"\"Server-side handlers for {name} service operations.\"\"\"\n"
        ));
        let server_inbound: Vec<&CsilServiceOperation> = service
            .operations
            .iter()
            .filter(|op| {
                matches!(
                    op.direction,
                    CsilServiceDirection::Unidirectional | CsilServiceDirection::Bidirectional
                )
            })
            .collect();
        if server_inbound.is_empty() {
            // ABC must have a body; reverse-only services have nothing here.
            out.push_str("    pass\n");
        } else {
            for op in &server_inbound {
                let method_name = op.name.to_case(Case::Snake);
                let input_type = self.map_type_expression(&op.input_type)?;
                out.push('\n');
                out.push_str("    @abstractmethod\n");
                match op.direction {
                    CsilServiceDirection::Unidirectional => {
                        let output_type = self.map_type_expression(&op.output_type)?;
                        out.push_str(&format!(
                            "    def {method_name}(self, req: {input_type}, ctx: dict) -> {output_type}:\n"
                        ));
                    }
                    CsilServiceDirection::Bidirectional => {
                        // Fire-and-forget channel inbound: the implementer's
                        // connection plumbing pulls a frame, the router decodes
                        // it, and this method handles it.
                        out.push_str(&format!(
                            "    def {method_name}(self, msg: {input_type}, ctx: dict) -> None:\n"
                        ));
                    }
                    CsilServiceDirection::Reverse => unreachable!(),
                }
                if op.doc_comments.is_empty() {
                    out.push_str(&format!("        \"\"\"{}\"\"\"\n", op.name));
                } else {
                    out.push_str("        \"\"\"");
                    for (i, line) in op.doc_comments.iter().enumerate() {
                        if i > 0 {
                            out.push_str("\n        ");
                        }
                        out.push_str(line);
                    }
                    out.push_str("\"\"\"\n");
                }
                out.push_str("        ...\n");
            }
        }
        out.push('\n');

        if Self::service_has_channel_ops(service) {
            // Channel router: only <-> dispatches inbound on the server side.
            let route_fn = format!("route_{}_channel", name.to_case(Case::Snake));
            let bidi_ops: Vec<&CsilServiceOperation> = service
                .operations
                .iter()
                .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
                .collect();

            out.push_str(&format!(
                "def {route_fn}(handlers: {handler_class}, codec: Codec, method: str, data: bytes, ctx: dict) -> None:\n"
            ));
            out.push_str(&format!(
                "    \"\"\"Decode one inbound channel frame for {name} and dispatch.\n\n\
                 \x20   The implementer feeds frames pulled off its connection here; this\n\
                 \x20   function never touches the wire.\n\
                 \x20   \"\"\"\n"
            ));
            if bidi_ops.is_empty() {
                // A reverse-only service still gets a router so consumers can
                // always call it, but any incoming method is a protocol error.
                out.push_str("    raise ServiceError(404, f\"unknown channel {method}\")\n\n");
            } else {
                for op in &bidi_ops {
                    let wire = Self::wire_method(&op.name);
                    let method_name = op.name.to_case(Case::Snake);
                    let input_type = self.map_type_expression(&op.input_type)?;
                    out.push_str(&format!("    if method == \"{wire}\":\n"));
                    out.push_str(&format!("        msg = codec.decode(data, {input_type})\n"));
                    out.push_str(&format!("        handlers.{method_name}(msg, ctx)\n"));
                    out.push_str("        return\n");
                }
                out.push_str("    raise ServiceError(404, f\"unknown channel {method}\")\n\n");
            }

            // Outbound encoders for <-> and <- (server pushes Output to client).
            for op in &service.operations {
                if !matches!(
                    op.direction,
                    CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
                ) {
                    continue;
                }
                let method_name = op.name.to_case(Case::Snake);
                let output_type = self.map_type_expression(&op.output_type)?;
                let wire = Self::wire_method(&op.name);
                let fn_name = format!("encode_{}_{}", name.to_case(Case::Snake), method_name);
                out.push_str(&format!(
                    "def {fn_name}(codec: Codec, msg: {output_type}) -> Tuple[str, bytes]:\n"
                ));
                out.push_str(&format!(
                    "    \"\"\"Encode a `{wire}` message the server pushes to a peer.\n\n\
                     \x20   Returns (method, bytes) for the implementer to frame on its connection.\n\
                     \x20   \"\"\"\n"
                ));
                out.push_str(&format!("    return (\"{wire}\", codec.encode(msg))\n\n"));
            }
        }

        Ok(out)
    }

    /// PascalCase wire method name — same convention as TS/Rust/Go so a CBOR
    /// or JSON frame keyed by method is routable across all generated targets.
    fn wire_method(s: &str) -> String {
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

    fn map_type_expression(&self, type_expr: &CsilTypeExpression) -> Result<String> {
        match type_expr {
            CsilTypeExpression::Builtin(name) => self.map_builtin_type(name),
            CsilTypeExpression::Reference(name) => Ok(name.to_case(Case::Pascal)),
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                let element = self.map_type_expression(element_type)?;
                match occurrence {
                    Some(CsilOccurrence::Optional) => Ok(format!("Optional[List[{element}]]")),
                    _ => Ok(format!("List[{element}]")),
                }
            }
            CsilTypeExpression::Map {
                key,
                value,
                occurrence,
            } => {
                let key_type = self.map_type_expression(key)?;
                let value_type = self.map_type_expression(value)?;
                match occurrence {
                    Some(CsilOccurrence::Optional) => {
                        Ok(format!("Optional[Dict[{key_type}, {value_type}]]"))
                    }
                    _ => Ok(format!("Dict[{key_type}, {value_type}]")),
                }
            }
            CsilTypeExpression::Group(_group) => Ok("Dict[str, Any]".to_string()),
            CsilTypeExpression::Choice(choices) => {
                let choice_types: Result<Vec<String>> = choices
                    .iter()
                    .map(|choice| self.map_type_expression(choice))
                    .collect();
                let choice_types = choice_types?;
                Ok(format!("Union[{}]", choice_types.join(", ")))
            }
            CsilTypeExpression::Literal(literal) => match literal {
                CsilLiteralValue::Integer(_) => Ok("int".to_string()),
                CsilLiteralValue::Float(_) => Ok("float".to_string()),
                CsilLiteralValue::Text(_) => Ok("str".to_string()),
                CsilLiteralValue::Bytes(_) => Ok("bytes".to_string()),
                CsilLiteralValue::Bool(_) => Ok("bool".to_string()),
                CsilLiteralValue::Null => Ok("None".to_string()),
                CsilLiteralValue::Array(_) => Ok("List[Any]".to_string()),
            },
            CsilTypeExpression::Range { .. } => Ok("int".to_string()),
            CsilTypeExpression::Socket(_) => Ok("Any".to_string()),
            CsilTypeExpression::Plug(_) => Ok("Any".to_string()),
            CsilTypeExpression::Constrained { base_type, .. } => {
                // For constrained types, use the base type
                self.map_type_expression(base_type)
            }
        }
    }

    fn map_builtin_type(&self, builtin: &str) -> Result<String> {
        let python_type = match builtin {
            "int" | "uint" => "int",
            "float" | "double" => "float",
            "text" | "tstr" => "str",
            "bytes" | "bstr" => "bytes",
            "bool" => "bool",
            "null" | "nil" => "None",
            "any" => "Any",
            _ => {
                return Err(CsilgenError::GenerationError(format!(
                    "Unknown builtin type: {builtin}"
                )));
            }
        };
        Ok(python_type.to_string())
    }

    fn generate_types_file(&self, types_code: String) -> Result<GeneratedFile> {
        let mut content = String::new();

        content.push_str("# Generated types from CSIL specification\n");
        content.push_str("# Do not edit this file manually\n\n");

        for import in &self.imports {
            content.push_str(import);
            content.push('\n');
        }

        content.push_str("\n\n");
        content.push_str(&types_code);

        Ok(GeneratedFile {
            path: "types.py".to_string(),
            content,
        })
    }

    fn generate_module_file(&self, body_code: String, want_client: bool) -> Result<GeneratedFile> {
        let (path, banner) = if want_client {
            (
                "client.py",
                "# Generated service clients from CSIL specification\n",
            )
        } else {
            (
                "services.py",
                "# Generated service handlers from CSIL specification\n",
            )
        };

        let mut content = String::new();
        content.push_str(banner);
        content.push_str("# Do not edit this file manually\n\n");

        for import in &self.imports {
            content.push_str(import);
            content.push('\n');
        }
        content.push_str("from .types import *\n");

        content.push_str("\n\n");
        content.push_str(&body_code);

        Ok(GeneratedFile {
            path: path.to_string(),
            content,
        })
    }

    fn generate_init_file(&self, files: &[GeneratedFile]) -> Result<GeneratedFile> {
        let mut content = String::new();

        content.push_str("# Generated package init from CSIL specification\n");
        content.push_str("# Do not edit this file manually\n\n");

        let mut exports = Vec::new();

        for file in files {
            if file.path == "types.py" {
                content.push_str("from .types import *\n");
                exports.push("types");
            } else if file.path == "services.py" {
                content.push_str("from .services import *\n");
                exports.push("services");
            } else if file.path == "client.py" {
                content.push_str("from .client import *\n");
                exports.push("client");
            }
        }

        if !exports.is_empty() {
            content.push_str(&format!(
                "\n__all__ = [{}]\n",
                exports
                    .iter()
                    .map(|e| format!("\"{e}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        Ok(GeneratedFile {
            path: "__init__.py".to_string(),
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{CsilRule, CsilRuleType, CsilSpecSerialized};
    use std::collections::HashMap;

    fn create_test_config(use_pydantic: bool) -> GeneratorConfig {
        let mut options = HashMap::new();
        options.insert(
            "use_pydantic".to_string(),
            serde_json::Value::Bool(use_pydantic),
        );

        GeneratorConfig {
            target: "python".to_string(),
            output_dir: "/tmp/test".to_string(),
            options,
        }
    }

    fn create_test_position() -> csilgen_common::CsilPosition {
        csilgen_common::CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        }
    }

    #[test]
    fn test_generate_simple_dataclass() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("email".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        assert_eq!(result.len(), 2); // types.py and __init__.py

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("@dataclass"));
        assert!(types_file.content.contains("class User:"));
        assert!(types_file.content.contains("name: str"));
        assert!(types_file.content.contains("email: Optional[str] = None"));
        assert!(types_file.content.contains("def to_dict"));
        assert!(types_file.content.contains("def from_dict"));
    }

    #[test]
    fn test_generate_pydantic_model() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            CsilFieldMetadata::Description("User's full name".to_string()),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(1)),
                        ],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        };

        let config = create_test_config(true);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(
            types_file
                .content
                .contains("from pydantic import BaseModel")
        );
        assert!(types_file.content.contains("class User(BaseModel):"));
        assert!(types_file.content.contains("name: str = Field"));
        assert!(
            types_file
                .content
                .contains("description=\"User's full name\"")
        );
        assert!(types_file.content.contains("min_length=1"));
    }

    #[test]
    fn unidirectional_service_emits_handlers_abc_no_router() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "UserService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "create_user".to_string(),
                        input_type: CsilTypeExpression::Builtin("text".to_string()),
                        output_type: CsilTypeExpression::Builtin("text".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: create_test_position(),
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        let services_file = result.iter().find(|f| f.path == "services.py").unwrap();
        let content = &services_file.content;

        // ServiceError exception always emitted alongside any service.
        assert!(content.contains("class ServiceError(Exception):"));
        // No Codec when there are no channel ops.
        assert!(!content.contains("class Codec(Protocol):"));

        // Server-side handlers ABC; reverse/bidi-free service has only the
        // unary ABC method, no channel router, no encoders.
        assert!(content.contains("class UserServiceHandlers(ABC):"));
        assert!(content.contains("def create_user(self, req: str, ctx: dict) -> str:"));
        assert!(!content.contains("route_user_service_channel"));
        assert!(!content.contains("encode_user_service_create_user"));

        // The legacy Client/Server/dispatch shape must NOT reappear.
        assert!(!content.contains("UserServiceClient"));
        assert!(!content.contains("UserServiceServer"));
        assert!(!content.contains("def dispatch(self, operation: str"));
    }

    #[test]
    fn bidirectional_op_emits_channel_inbound_router_and_outbound_encoder() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Match".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![
                        CsilServiceOperation {
                            name: "list_events".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Builtin("text".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                        },
                        CsilServiceOperation {
                            name: "play".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Builtin("text".to_string()),
                            direction: CsilServiceDirection::Bidirectional,
                            position: create_test_position(),
                            doc_comments: vec!["Open a play channel.".to_string()],
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;

        // Codec protocol emitted exactly once at the top of the services file.
        assert!(content.contains("class Codec(Protocol):"));
        assert_eq!(content.matches("class Codec(Protocol):").count(), 1);

        // Handlers ABC contains both unidirectional (returns Output) and
        // bidirectional inbound (fire-and-forget, returns None).
        assert!(content.contains("class MatchHandlers(ABC):"));
        assert!(content.contains("def list_events(self, req: str, ctx: dict) -> str:"));
        assert!(content.contains("def play(self, msg: str, ctx: dict) -> None:"));
        // Doc comment surfaces as the method docstring.
        assert!(content.contains("\"\"\"Open a play channel.\"\"\""));

        // Router routes inbound by wire-method name (PascalCase, matches
        // TS/Rust/Go so frames are cross-language compatible).
        assert!(content.contains(
            "def route_match_channel(handlers: MatchHandlers, codec: Codec, method: str, data: bytes, ctx: dict) -> None:"
        ));
        assert!(content.contains("if method == \"Play\":"));
        assert!(content.contains("msg = codec.decode(data, str)"));
        assert!(content.contains("handlers.play(msg, ctx)"));
        assert!(content.contains("raise ServiceError(404, f\"unknown channel {method}\")"));

        // Outbound encoder for the bidirectional op (server pushes Output).
        assert!(
            content.contains("def encode_match_play(codec: Codec, msg: str) -> Tuple[str, bytes]:")
        );
        assert!(content.contains("return (\"Play\", codec.encode(msg))"));
    }

    #[test]
    fn reverse_op_emits_only_outbound_encoder_no_handler_no_router_case() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Callbacks".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "notify".to_string(),
                        input_type: CsilTypeExpression::Builtin("text".to_string()),
                        output_type: CsilTypeExpression::Builtin("text".to_string()),
                        direction: CsilServiceDirection::Reverse,
                        position: create_test_position(),
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;

        // Reverse-only service: ABC body is `pass` (no inbound methods).
        assert!(content.contains("class CallbacksHandlers(ABC):"));
        assert!(content.contains("    pass\n"));
        // No inbound method named `notify` on the server side.
        assert!(!content.contains("def notify(self, "));

        // Router still exists for API consistency but has no `Notify` case.
        assert!(content.contains("def route_callbacks_channel("));
        let router_start = content.find("def route_callbacks_channel(").unwrap();
        let router_body = &content[router_start..];
        assert!(!router_body.contains("if method == \"Notify\":"));

        // The server-pushed encoder is present.
        assert!(
            content.contains(
                "def encode_callbacks_notify(codec: Codec, msg: str) -> Tuple[str, bytes]:"
            )
        );
        assert!(content.contains("return (\"Notify\", codec.encode(msg))"));
    }

    #[test]
    fn test_field_visibility_handling() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Message".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("content".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::Bidirectional,
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("timestamp".to_string())),
                            value_type: CsilTypeExpression::Builtin("int".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 2,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        // The to_dict method should exclude receive-only fields
        assert!(types_file.content.contains("def to_dict"));
        // The from_dict method should include receive-only fields
        assert!(types_file.content.contains("def from_dict"));
    }

    #[test]
    fn test_field_dependencies() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "ConditionalData".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("type".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("extra_data".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::DependsOn {
                                field: "type".to_string(),
                                value: Some(CsilLiteralValue::Text("advanced".to_string())),
                            }],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("def validate(self)"));
        assert!(
            types_file
                .content
                .contains("Field 'extra_data' requires 'type' to be \"advanced\"")
        );
        assert!(types_file.content.contains("def __post_init__(self)"));
    }

    #[test]
    fn test_type_mappings() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "TypeTest".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("numbers".to_string())),
                            value_type: CsilTypeExpression::Array {
                                element_type: Box::new(CsilTypeExpression::Builtin(
                                    "int".to_string(),
                                )),
                                occurrence: None,
                            },
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("mapping".to_string())),
                            value_type: CsilTypeExpression::Map {
                                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                                value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                                occurrence: None,
                            },
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("numbers: List[int]"));
        assert!(types_file.content.contains("mapping: Dict[str, int]"));
    }

    #[test]
    fn test_union_types() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "StringOrNumber".to_string(),
                rule_type: CsilRuleType::TypeChoice(vec![
                    CsilTypeExpression::Builtin("text".to_string()),
                    CsilTypeExpression::Builtin("int".to_string()),
                ]),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(
            types_file
                .content
                .contains("StringOrNumber = Union[str, int]")
        );
    }

    #[test]
    fn test_python_naming_conventions() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "test-class".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("field-name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("class TestClass:"));
        assert!(types_file.content.contains("field_name: str"));
    }

    #[test]
    fn test_empty_spec() {
        let spec = CsilSpecSerialized {
            rules: vec![],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn test_init_file_generation() {
        let spec = CsilSpecSerialized {
            rules: vec![
                CsilRule {
                    name: "User".to_string(),
                    rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                },
                CsilRule {
                    name: "UserService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![],
                    }),
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                },
            ],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        // Should have types.py, services.py, and __init__.py
        assert_eq!(result.len(), 3);

        let init_file = result.iter().find(|f| f.path == "__init__.py").unwrap();
        assert!(init_file.content.contains("from .types import *"));
        assert!(init_file.content.contains("from .services import *"));
        assert!(
            init_file
                .content
                .contains("__all__ = [\"types\", \"services\"]")
        );
    }

    #[test]
    fn test_typedef_group_emits_dataclass_not_dict_alias() {
        // `Task = { ... }` parses to a TypeDef carrying a Group; it must become a
        // real dataclass, not a bare `Dict[str, Any]` alias.
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Task".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("uuid".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("payload".to_string())),
                            value_type: CsilTypeExpression::Builtin("bytes".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("@dataclass"));
        assert!(types_file.content.contains("class Task:"));
        assert!(types_file.content.contains("uuid: str"));
        assert!(types_file.content.contains("payload: bytes"));
        assert!(!types_file.content.contains("Task = Dict[str, Any]"));
    }

    fn service_spec_with_union_op() -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "CorndogsService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "SubmitTask".to_string(),
                        input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                        output_type: CsilTypeExpression::Choice(vec![
                            CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                            CsilTypeExpression::Reference("ServiceError".to_string()),
                        ]),
                        direction: CsilServiceDirection::Unidirectional,
                        position: create_test_position(),
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    #[test]
    fn test_python_client_target_emits_typed_client() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();

        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        let client = result
            .iter()
            .find(|f| f.path == "client.py")
            .expect("client.py emitted");
        assert!(client.content.contains("class Transport(Protocol):"));
        assert!(client.content.contains("class CorndogsClient:"));
        // Success type is stripped from the `/ ServiceError` union.
        assert!(
            client
                .content
                .contains("def submit_task(self, req: SubmitTaskRequest) -> SubmitTaskResponse:")
        );
        assert!(
            client
                .content
                .contains("return self._transport.call(\"corndogs\", \"SubmitTask\", req)")
        );
        // The server handler surface must not be emitted for the client target.
        assert!(!result.iter().any(|f| f.path == "services.py"));
    }

    #[test]
    fn test_python_server_alias_and_typesonly() {
        let spec = service_spec_with_union_op();

        let mut config = create_test_config(false);
        config.target = "python-server".to_string();
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        assert!(result.iter().any(|f| f.path == "services.py"));
        assert!(!result.iter().any(|f| f.path == "client.py"));

        let mut config = create_test_config(false);
        config.target = "python-typesonly".to_string();
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        assert!(!result.iter().any(|f| f.path == "services.py"));
        assert!(!result.iter().any(|f| f.path == "client.py"));
    }

    #[test]
    fn test_unknown_python_subtarget_errors() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-bogus".to_string();
        assert!(generate_python_code_from_serialized(&spec, &config).is_err());
    }
}
