//! OpenAPI specification generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target openapi` from
//! `csilgen_openapi_generator.wasm`. Operates directly on
//! `csilgen_common::CsilSpecSerialized` like the other live generators — no
//! `csilgen-core` dependency, no `Serialized → Core` shim.

mod wasm;

use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilFieldVisibility, CsilGroupEntry, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation,
    CsilSizeConstraint, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    CsilgenError, GeneratedFile, GeneratedFiles, GeneratorConfig, GeneratorWarning, Result,
    WarningLevel,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Generate an OpenAPI specification from a CSIL specification, along with any
/// warnings raised during generation (e.g. operations OpenAPI cannot natively
/// model — see `generate_operation_path`).
pub fn generate_openapi_spec(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<(GeneratedFiles, Vec<GeneratorWarning>)> {
    let mut generator = OpenApiGenerator::new(config);
    let files = generator.generate(spec)?;
    Ok((files, generator.warnings))
}

struct OpenApiGenerator {
    config: GeneratorConfig,
    warnings: Vec<GeneratorWarning>,
}

impl OpenApiGenerator {
    fn new(config: &GeneratorConfig) -> Self {
        Self {
            config: config.clone(),
            warnings: Vec::new(),
        }
    }

    fn generate(&mut self, spec: &CsilSpecSerialized) -> Result<GeneratedFiles> {
        // Validate options before doing any work so a misconfiguration fails the
        // whole run rather than silently degrading — the same validate-early
        // idiom the other generators use so a bad option behaves identically
        // across targets.
        self.validate_options()?;

        let mut openapi_spec = json!({
            "openapi": "3.0.3",
            "info": {
                "title": self.config.options.get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Generated API"),
                "version": self.config.options.get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1.0.0"),
                "description": self.config.options.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("API generated from CSIL specification")
            },
            "components": {
                "schemas": {}
            },
            "paths": {}
        });

        let mut type_definitions = HashMap::new();
        let mut services = Vec::new();

        for rule in &spec.rules {
            match &rule.rule_type {
                CsilRuleType::TypeDef(type_expr) => {
                    type_definitions.insert(rule.name.clone(), type_expr.clone());
                }
                CsilRuleType::GroupDef(group_expr) => {
                    type_definitions.insert(
                        rule.name.clone(),
                        CsilTypeExpression::Group(group_expr.clone()),
                    );
                }
                CsilRuleType::ServiceDef(service_def) => {
                    services.push((rule.name.clone(), service_def));
                }
                CsilRuleType::TypeChoice(choices) => {
                    type_definitions.insert(
                        rule.name.clone(),
                        CsilTypeExpression::Choice(choices.clone()),
                    );
                }
                CsilRuleType::GroupChoice(groups) => {
                    if let Some(first_group) = groups.first() {
                        type_definitions.insert(
                            rule.name.clone(),
                            CsilTypeExpression::Group(first_group.clone()),
                        );
                    }
                }
            }
        }

        self.generate_schemas(&mut openapi_spec, &type_definitions)?;

        self.generate_paths(&mut openapi_spec, &services, &type_definitions)?;

        let content = serde_json::to_string_pretty(&openapi_spec).map_err(|e| {
            csilgen_common::CsilgenError::GenerationError(format!(
                "Failed to serialize OpenAPI spec: {e}"
            ))
        })?;

        Ok(vec![GeneratedFile {
            path: "openapi.json".to_string(),
            content,
        }])
    }

    /// The `decimal_mapping` option only selects the in-memory type a *code*
    /// generator emits; the OpenAPI schema is identical (`decimal` is exact text
    /// either way). We still validate it here so an unknown value is a hard
    /// error rather than a silent no-op, keeping behavior consistent with the
    /// code generators that do act on it (per the wire contract).
    fn validate_options(&self) -> Result<()> {
        match self.config.options.get("decimal_mapping") {
            None => Ok(()),
            Some(value) => match value.as_str() {
                Some("csil") | Some("library") => Ok(()),
                Some(other) => Err(CsilgenError::GenerationError(format!(
                    "decimal_mapping must be \"csil\" or \"library\", got {other:?}"
                ))),
                None => Err(CsilgenError::GenerationError(format!(
                    "decimal_mapping must be a string, got {value:?}"
                ))),
            },
        }
    }

    fn generate_schemas(
        &self,
        openapi_spec: &mut Value,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<()> {
        let schemas = openapi_spec
            .get_mut("components")
            .and_then(|c| c.get_mut("schemas"))
            .unwrap();

        for (name, type_expr) in type_definitions {
            let schema = self.type_expression_to_schema(type_expr, type_definitions)?;
            schemas[name] = schema;
        }

        Ok(())
    }

    fn generate_paths(
        &mut self,
        openapi_spec: &mut Value,
        services: &[(String, &CsilServiceDefinition)],
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<()> {
        // Purely additive: a service carrying `@wire-id(N)` surfaces its ordinal
        // as an OpenAPI tag (the service-level object) with the `x-csil-wire-id`
        // vendor extension. Wire-id-free specs emit no `tags` and stay unchanged.
        for (service_name, service_def) in services {
            if let Some(wire_id) = service_def.wire_id {
                let tags = openapi_spec
                    .as_object_mut()
                    .unwrap()
                    .entry("tags")
                    .or_insert_with(|| json!([]));
                tags.as_array_mut().unwrap().push(json!({
                    "name": service_name,
                    "x-csil-wire-id": wire_id,
                }));
            }
        }

        let paths = openapi_spec.get_mut("paths").unwrap();

        for (service_name, service_def) in services {
            for operation in &service_def.operations {
                self.generate_operation_path(paths, service_name, operation, type_definitions)?;
            }
        }

        Ok(())
    }

    fn generate_operation_path(
        &mut self,
        paths: &mut Value,
        service_name: &str,
        operation: &CsilServiceOperation,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<()> {
        let path = format!("/{}/{}", service_name.to_lowercase(), operation.name);
        // OpenAPI describes HTTP. `->` maps to a request/response POST cleanly;
        // `<->` and `<-` need a persistent channel that OpenAPI cannot model
        // natively, so we surface a warning and tag the operation with
        // `x-csil-direction` so consumers know the HTTP shape is a documentation
        // placeholder rather than a faithful spec.
        let (method, csil_direction) = match operation.direction {
            CsilServiceDirection::Unidirectional => ("post", None),
            CsilServiceDirection::Bidirectional => ("post", Some("bidirectional")),
            CsilServiceDirection::Reverse => ("get", Some("reverse")),
        };

        if let Some(direction_label) = csil_direction {
            self.warnings.push(GeneratorWarning {
                level: WarningLevel::Warning,
                message: format!(
                    "{service_name}.{op}: OpenAPI doesn't natively model {direction_label} \
                     ({arrow}) operations; the generated HTTP {method} is a documentation \
                     placeholder. The real semantics require a persistent channel (e.g. \
                     WebSocket). The operation carries `x-csil-direction: \"{direction_label}\"` \
                     so consumers can detect it.",
                    op = operation.name,
                    arrow = match operation.direction {
                        CsilServiceDirection::Bidirectional => "<->",
                        CsilServiceDirection::Reverse => "<-",
                        CsilServiceDirection::Unidirectional => unreachable!(),
                    },
                ),
                location: None,
                suggestion: Some(
                    "Use a code target (rust/go/typescript/python) to generate the actual \
                     channel handlers + router for this operation."
                        .to_string(),
                ),
            });
        }

        // A push-only operation (`op: <- Event`) carries no input — its
        // `input_type` is the `null` builtin. Emitting a `null`-typed, required
        // request body would force callers to send an explicit JSON null, so we
        // omit the body entirely and let the operation describe a payload-less
        // request.
        let request_body = if Self::is_null_type(&operation.input_type) {
            None
        } else {
            let request_schema =
                self.type_expression_to_schema(&operation.input_type, type_definitions)?;
            Some(json!({
                "required": true,
                "content": {
                    "application/json": {
                        "schema": request_schema
                    }
                }
            }))
        };
        let response_schema =
            self.type_expression_to_schema(&operation.output_type, type_definitions)?;

        let mut operation_spec = json!({
            "summary": format!("{} operation", operation.name),
            "operationId": format!("{}_{}", service_name.to_lowercase(), operation.name),
            "responses": {
                "200": {
                    "description": "Successful response",
                    "content": {
                        "application/json": {
                            "schema": response_schema
                        }
                    }
                },
                "400": {
                    "description": "Bad Request"
                },
                "500": {
                    "description": "Internal Server Error"
                }
            }
        });
        if let Some(body) = request_body {
            operation_spec["requestBody"] = body;
        }
        if let Some(direction_label) = csil_direction {
            operation_spec["x-csil-direction"] = json!(direction_label);
        }
        if let Some(wire_id) = operation.wire_id {
            operation_spec["x-csil-wire-id"] = json!(wire_id);
        }

        if paths.get(&path).is_none() {
            paths[&path] = json!({});
        }

        paths[&path][method] = operation_spec;

        Ok(())
    }

    fn type_expression_to_schema(
        &self,
        type_expr: &CsilTypeExpression,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<Value> {
        match type_expr {
            CsilTypeExpression::Builtin(builtin_type) => {
                Ok(self.builtin_type_to_schema(builtin_type))
            }
            CsilTypeExpression::Reference(ref_name) => Ok(json!({
                "$ref": format!("#/components/schemas/{}", ref_name)
            })),
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                let items_schema =
                    self.type_expression_to_schema(element_type, type_definitions)?;
                let mut array_schema = json!({
                    "type": "array",
                    "items": items_schema
                });

                if let Some(occ) = occurrence {
                    self.add_occurrence_constraints(&mut array_schema, occ);
                }

                Ok(array_schema)
            }
            CsilTypeExpression::Map {
                key: _,
                value,
                occurrence,
            } => {
                let additional_properties =
                    self.type_expression_to_schema(value, type_definitions)?;
                let mut object_schema = json!({
                    "type": "object",
                    "additionalProperties": additional_properties
                });

                if let Some(occ) = occurrence {
                    self.add_occurrence_constraints(&mut object_schema, occ);
                }

                Ok(object_schema)
            }
            CsilTypeExpression::Group(group_expr) => {
                let mut properties = Map::new();
                let mut required = Vec::new();

                for entry in &group_expr.entries {
                    if let Some(key) = &entry.key {
                        let property_name = self.group_key_to_string(key);
                        let property_schema =
                            self.generate_property_schema(entry, type_definitions)?;

                        properties.insert(property_name.clone(), property_schema);

                        if entry.occurrence.is_none()
                            || !self.is_optional_occurrence(&entry.occurrence)
                        {
                            required.push(Value::String(property_name));
                        }
                    }
                }

                let mut schema = json!({
                    "type": "object",
                    "properties": properties
                });

                if !required.is_empty() {
                    schema["required"] = Value::Array(required);
                }

                Ok(schema)
            }
            // OpenAPI 3.0 has no native tuple, so a fixed-shape array is modeled
            // as a length-pinned array (`minItems == maxItems == N`) whose
            // `items` is the union of the distinct entry value-type schemas. We
            // deliberately avoid 3.1's `prefixItems` so the output validates
            // against the `3.0.3` document this generator declares. Each entry's
            // value type reuses the same converter as everything else, so keyed
            // tuples (`[tag: text, value: any]`) and bare tuples (`[text, int]`)
            // behave identically — only the value type matters to the schema.
            CsilTypeExpression::Tuple(group_expr) => {
                let arity = group_expr.entries.len();
                let mut distinct: Vec<Value> = Vec::new();
                for entry in &group_expr.entries {
                    let entry_schema =
                        self.type_expression_to_schema(&entry.value_type, type_definitions)?;
                    if !distinct.contains(&entry_schema) {
                        distinct.push(entry_schema);
                    }
                }
                // A `oneOf` requires at least one branch and is pointless for a
                // single distinct type, so collapse those cases to a plain
                // schema (or an unconstrained schema for the empty tuple).
                let items = match distinct.len() {
                    0 => json!({}),
                    1 => distinct.pop().unwrap(),
                    _ => json!({ "oneOf": distinct }),
                };
                Ok(json!({
                    "type": "array",
                    "minItems": arity,
                    "maxItems": arity,
                    "items": items
                }))
            }
            CsilTypeExpression::Choice(choices) => {
                let choice_schemas: Result<Vec<Value>> = choices
                    .iter()
                    .map(|choice| self.type_expression_to_schema(choice, type_definitions))
                    .collect();

                Ok(json!({
                    "oneOf": choice_schemas?
                }))
            }
            CsilTypeExpression::Range {
                start,
                end,
                inclusive: _,
            } => {
                let mut schema = json!({
                    "type": "integer"
                });

                if let Some(min) = start {
                    schema["minimum"] = json!(min);
                }

                if let Some(max) = end {
                    schema["maximum"] = json!(max);
                }

                Ok(schema)
            }
            CsilTypeExpression::Literal(literal) => Ok(self.literal_to_schema(literal)),
            CsilTypeExpression::Socket(_) | CsilTypeExpression::Plug(_) => Ok(json!({})),
            CsilTypeExpression::Constrained {
                base_type,
                constraints,
            } => {
                let mut schema = self.type_expression_to_schema(base_type, type_definitions)?;

                // Apply constraints to the schema
                for constraint in constraints {
                    match constraint {
                        CsilControlOperator::Size(size_constraint) => {
                            match size_constraint {
                                CsilSizeConstraint::Exact(val) => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(val);
                                        schema["maxLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(val);
                                        schema["maxItems"] = json!(val);
                                    }
                                }
                                CsilSizeConstraint::Range { min, max } => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(min);
                                        schema["maxLength"] = json!(max);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(min);
                                        schema["maxItems"] = json!(max);
                                    }
                                }
                                CsilSizeConstraint::Min(val) => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(val);
                                    }
                                }
                                CsilSizeConstraint::Max(val) => {
                                    if schema["type"] == "string" {
                                        schema["maxLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["maxItems"] = json!(val);
                                    }
                                }
                            }
                        }
                        CsilControlOperator::Regex(pattern)
                            if schema["type"] == "string" => {
                                schema["pattern"] = json!(pattern);
                            }
                        CsilControlOperator::Default(value) => {
                            schema["default"] = self.literal_to_json_value(value);
                        }
                        CsilControlOperator::GreaterEqual(value) => {
                            self.apply_comparison_bound(
                                &mut schema,
                                "minimum",
                                "x-csil-minimum",
                                None,
                                value,
                            );
                        }
                        CsilControlOperator::LessEqual(value) => {
                            self.apply_comparison_bound(
                                &mut schema,
                                "maximum",
                                "x-csil-maximum",
                                None,
                                value,
                            );
                        }
                        CsilControlOperator::GreaterThan(value) => {
                            self.apply_comparison_bound(
                                &mut schema,
                                "minimum",
                                "x-csil-exclusive-minimum",
                                Some("exclusiveMinimum"),
                                value,
                            );
                        }
                        CsilControlOperator::LessThan(value) => {
                            self.apply_comparison_bound(
                                &mut schema,
                                "maximum",
                                "x-csil-exclusive-maximum",
                                Some("exclusiveMaximum"),
                                value,
                            );
                        }
                        CsilControlOperator::Json
                            // For .json constraint in OpenAPI, we can use format
                            if schema["type"] == "string" => {
                                schema["format"] = json!("json");
                            }
                        CsilControlOperator::Cbor
                            // For .cbor constraint in OpenAPI, we can use format
                            if schema["type"] == "string" => {
                                schema["format"] = json!("byte");
                                schema["contentMediaType"] = json!("application/cbor");
                            }
                        CsilControlOperator::Cborseq
                            // For .cborseq constraint in OpenAPI, we can use format
                            if schema["type"] == "string" => {
                                schema["format"] = json!("byte");
                                schema["contentMediaType"] = json!("application/cbor-seq");
                            }
                        // `.eq` pins the value to a single constant; JSON Schema
                        // `const` is the direct equivalent.
                        CsilControlOperator::Equal(value) => {
                            schema["const"] = self.literal_to_json_value(value);
                        }
                        // `.ne` excludes a single value; the negation of `const`
                        // expresses exactly that.
                        CsilControlOperator::NotEqual(value) => {
                            schema["not"] = json!({ "const": self.literal_to_json_value(value) });
                        }
                        // JSON Schema has no bit-field facet, so preserve the CSIL
                        // intent in a vendor extension rather than dropping it.
                        CsilControlOperator::Bits(bits) => {
                            schema["x-csil-bits"] = json!(bits);
                        }
                        // `.and` intersects with another type; `allOf` is the JSON
                        // Schema intersection, folding the current schema in so its
                        // already-applied facets are not lost.
                        CsilControlOperator::And(other) => {
                            let other_schema =
                                self.type_expression_to_schema(other, type_definitions)?;
                            Self::merge_all_of(&mut schema, other_schema);
                        }
                        // `.within` constrains the value to a member of another
                        // type. There is no single JSON Schema facet for "is a
                        // member of"; carry the membership type as a vendor
                        // extension so it survives round-tripping while keeping it
                        // distinct from `.and`'s hard intersection.
                        CsilControlOperator::Within(other) => {
                            let within_schema =
                                self.type_expression_to_schema(other, type_definitions)?;
                            schema["x-csil-within"] = within_schema;
                        }
                        // Remaining arms are the guarded string-only operators
                        // (`.regex`/`.json`/`.cbor*`) applied to a non-string base,
                        // where the facet simply does not apply.
                        _ => {}
                    }
                }

                Ok(schema)
            }
        }
    }

    fn generate_property_schema(
        &self,
        entry: &CsilGroupEntry,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<Value> {
        let mut schema = self.type_expression_to_schema(&entry.value_type, type_definitions)?;

        for metadata in &entry.metadata {
            match metadata {
                CsilFieldMetadata::Visibility(visibility) => match visibility {
                    CsilFieldVisibility::SendOnly => {
                        schema["description"] =
                            json!("Send-only field (not included in responses)");
                    }
                    CsilFieldVisibility::ReceiveOnly => {
                        schema["description"] =
                            json!("Receive-only field (not included in requests)");
                    }
                    CsilFieldVisibility::Bidirectional => {}
                },
                CsilFieldMetadata::Constraint(constraint) => {
                    self.add_validation_constraint(&mut schema, constraint);
                }
                CsilFieldMetadata::Description(desc) => {
                    schema["description"] = json!(desc);
                }
                CsilFieldMetadata::DependsOn { field, value } => {
                    let dependency_desc = if let Some(val) = value {
                        format!("Depends on field '{field}' having value '{val:?}'")
                    } else {
                        format!("Depends on field '{field}'")
                    };

                    if let Some(existing_desc) = schema.get("description") {
                        let combined = format!(
                            "{}. {}",
                            existing_desc.as_str().unwrap_or(""),
                            dependency_desc
                        );
                        schema["description"] = json!(combined);
                    } else {
                        schema["description"] = json!(dependency_desc);
                    }
                }
                // Boolean `@depends-on(...)` renders the same way as the simple
                // `DependsOn` form (a human-readable note appended to the field
                // description) so consumers don't have to learn a second
                // mechanism; only the rendered condition is richer.
                CsilFieldMetadata::DependsOnExpr(condition) => {
                    let dependency_desc =
                        format!("Depends on: {}", self.render_depends_condition(condition));
                    if let Some(existing_desc) = schema.get("description") {
                        let combined = format!(
                            "{}. {}",
                            existing_desc.as_str().unwrap_or(""),
                            dependency_desc
                        );
                        schema["description"] = json!(combined);
                    } else {
                        schema["description"] = json!(dependency_desc);
                    }
                }
                CsilFieldMetadata::Custom {
                    name,
                    parameters: _,
                } => {
                    if let Some(existing_desc) = schema.get("description") {
                        let combined = format!(
                            "{}. Custom hint: {}",
                            existing_desc.as_str().unwrap_or(""),
                            name
                        );
                        schema["description"] = json!(combined);
                    } else {
                        schema["description"] = json!(format!("Custom hint: {}", name));
                    }
                }
            }
        }

        if let Some(occ) = &entry.occurrence {
            self.add_occurrence_constraints(&mut schema, occ);
        }

        Ok(schema)
    }

    fn builtin_type_to_schema(&self, builtin_type: &str) -> Value {
        match builtin_type {
            "int" | "uint" | "nint" | "pint" => json!({
                "type": "integer"
            }),
            "float" | "float16" | "float32" | "float64" => json!({
                "type": "number"
            }),
            "text" | "tstr" => json!({
                "type": "string"
            }),
            "bytes" | "bstr" => json!({
                "type": "string",
                "format": "byte"
            }),
            "bool" => json!({
                "type": "boolean"
            }),
            // CBOR tag 0 (RFC 3339 date/time string); `date-time` is the
            // standard JSON Schema/OpenAPI string format for this.
            "timestamp" => json!({
                "type": "string",
                "format": "date-time"
            }),
            // CBOR tag 4 decimal fraction on the wire, but exact base-10 carried
            // as text in JSON so no float rounding is introduced.
            "decimal" => json!({
                "type": "string",
                "format": "decimal"
            }),
            "null" | "nil" => json!({
                "type": "null"
            }),
            "any" => json!({}),
            _ => json!({
                "type": "string",
                "description": format!("Custom type: {}", builtin_type)
            }),
        }
    }

    fn literal_to_schema(&self, literal: &CsilLiteralValue) -> Value {
        match literal {
            CsilLiteralValue::Integer(val) => json!({
                "type": "integer",
                "enum": [val]
            }),
            CsilLiteralValue::Float(val) => json!({
                "type": "number",
                "enum": [val]
            }),
            CsilLiteralValue::Text(val) => json!({
                "type": "string",
                "enum": [val]
            }),
            CsilLiteralValue::Bool(val) => json!({
                "type": "boolean",
                "enum": [val]
            }),
            CsilLiteralValue::Null => json!({
                "type": "null"
            }),
            CsilLiteralValue::Bytes(val) => json!({
                "type": "string",
                "format": "byte",
                "description": format!("Byte array of length {}", val.len())
            }),
            CsilLiteralValue::Array(elements) => {
                let json_elements: Vec<Value> = elements
                    .iter()
                    .map(|e| self.literal_to_json_value(e))
                    .collect();
                json!({
                    "type": "array",
                    "enum": [json_elements]
                })
            }
        }
    }

    fn literal_to_json_value(&self, literal: &CsilLiteralValue) -> Value {
        match literal {
            CsilLiteralValue::Integer(val) => json!(val),
            CsilLiteralValue::Float(val) => json!(val),
            CsilLiteralValue::Text(val) => json!(val),
            CsilLiteralValue::Bool(val) => json!(val),
            CsilLiteralValue::Null => json!(null),
            CsilLiteralValue::Bytes(val) => json!(val),
            CsilLiteralValue::Array(elements) => {
                let json_elements: Vec<Value> = elements
                    .iter()
                    .map(|e| self.literal_to_json_value(e))
                    .collect();
                Value::Array(json_elements)
            }
        }
    }

    /// Place a comparison bound using the keyword that matches the schema's
    /// type. Numeric schemas get the real `minimum`/`maximum` keyword (and the
    /// OAS-3.0 boolean `exclusiveMinimum`/`exclusiveMaximum` flag for `.gt`/`.lt`);
    /// the string-typed `decimal` (`format: decimal`) and `timestamp`
    /// (`format: date-time`) schemas instead carry the bound as a string in a
    /// vendor extension, since a string-valued numeric keyword would be an
    /// invalid OpenAPI schema and the constraint would otherwise be silently lost.
    fn apply_comparison_bound(
        &self,
        schema: &mut Value,
        numeric_key: &str,
        vendor_key: &str,
        exclusive_flag_key: Option<&str>,
        value: &CsilLiteralValue,
    ) {
        if schema.get("type").and_then(Value::as_str) == Some("string") {
            schema[vendor_key] = self.bound_as_string(value);
        } else if let Some(numeric) = self.numeric_bound(value) {
            schema[numeric_key] = numeric;
            if let Some(flag) = exclusive_flag_key {
                schema[flag] = json!(true);
            }
        }
    }

    /// Numeric bounds carry only as integers or floats; anything else (e.g. a
    /// `decimal`/`timestamp` text bound) is not a numeric keyword value. The
    /// actual conversion defers to `literal_to_json_value` so there is a single
    /// place that decides how a literal becomes JSON.
    fn numeric_bound(&self, value: &CsilLiteralValue) -> Option<Value> {
        match value {
            CsilLiteralValue::Integer(_) | CsilLiteralValue::Float(_) => {
                Some(self.literal_to_json_value(value))
            }
            _ => None,
        }
    }

    /// The vendor-extension form always carries the bound as text, since the
    /// whole reason for the extension is that the instance type is a string.
    /// Conversion defers to `literal_to_json_value`; a text bound is already a
    /// JSON string and passes through, while an integer decimal bound (e.g. `0`)
    /// is rendered as its decimal string (`"0"`) so it stays consistent with the
    /// text bounds it may sit alongside on the same string-typed field.
    fn bound_as_string(&self, value: &CsilLiteralValue) -> Value {
        match self.literal_to_json_value(value) {
            Value::String(s) => Value::String(s),
            other => json!(other.to_string()),
        }
    }

    /// Intersect `schema` with `other` under JSON Schema `allOf`. If `schema`
    /// is already an `allOf` (a prior `.and`), append rather than nest so
    /// repeated `.and` constraints stay a flat list instead of growing a tower
    /// of single-element wrappers.
    fn merge_all_of(schema: &mut Value, other: Value) {
        if let Some(all_of) = schema.get_mut("allOf").and_then(|v| v.as_array_mut()) {
            all_of.push(other);
        } else {
            let existing = schema.take();
            *schema = json!({ "allOf": [existing, other] });
        }
    }

    fn group_key_to_string(&self, key: &CsilGroupKey) -> String {
        match key {
            CsilGroupKey::Bare(name) => name.clone(),
            CsilGroupKey::Type(_) => "typed_key".to_string(),
            CsilGroupKey::Literal(literal) => match literal {
                CsilLiteralValue::Text(s) => s.clone(),
                CsilLiteralValue::Integer(i) => i.to_string(),
                _ => "literal_key".to_string(),
            },
        }
    }

    fn add_occurrence_constraints(&self, schema: &mut Value, occurrence: &CsilOccurrence) {
        match occurrence {
            CsilOccurrence::Optional => {}
            CsilOccurrence::ZeroOrMore => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(0);
                }
            }
            CsilOccurrence::OneOrMore => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(1);
                }
            }
            CsilOccurrence::Exact(count) => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(count);
                    schema["maxItems"] = json!(count);
                }
            }
            CsilOccurrence::Range { min, max } => {
                if schema["type"] == "array" {
                    if let Some(min_val) = min {
                        schema["minItems"] = json!(min_val);
                    }
                    if let Some(max_val) = max {
                        schema["maxItems"] = json!(max_val);
                    }
                }
            }
        }
    }

    fn add_validation_constraint(&self, schema: &mut Value, constraint: &CsilValidationConstraint) {
        match constraint {
            CsilValidationConstraint::MinLength(min) => {
                schema["minLength"] = json!(min);
            }
            CsilValidationConstraint::MaxLength(max) => {
                schema["maxLength"] = json!(max);
            }
            CsilValidationConstraint::MinItems(min) => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(min);
                } else if schema["type"] == "object" {
                    schema["minProperties"] = json!(min);
                }
            }
            CsilValidationConstraint::MaxItems(max) => {
                if schema["type"] == "array" {
                    schema["maxItems"] = json!(max);
                } else if schema["type"] == "object" {
                    schema["maxProperties"] = json!(max);
                }
            }
            CsilValidationConstraint::MinValue(value) => {
                self.apply_comparison_bound(schema, "minimum", "x-csil-minimum", None, value);
            }
            CsilValidationConstraint::MaxValue(value) => {
                self.apply_comparison_bound(schema, "maximum", "x-csil-maximum", None, value);
            }
            CsilValidationConstraint::Custom { name, value } => {
                let constraint_desc = format!("Custom constraint '{name}': {value:?}");
                if let Some(existing_desc) = schema.get("description") {
                    let combined = format!(
                        "{}. {}",
                        existing_desc.as_str().unwrap_or(""),
                        constraint_desc
                    );
                    schema["description"] = json!(combined);
                } else {
                    schema["description"] = json!(constraint_desc);
                }
            }
        }
    }

    fn is_optional_occurrence(&self, occurrence: &Option<CsilOccurrence>) -> bool {
        matches!(
            occurrence,
            Some(CsilOccurrence::Optional) | Some(CsilOccurrence::ZeroOrMore)
        )
    }

    /// A no-input operation's `input_type` is the `null`/`nil` builtin (or a
    /// literal null). It's matched here rather than inline so the "push-only
    /// means no request body" decision lives in one place.
    fn is_null_type(type_expr: &CsilTypeExpression) -> bool {
        matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
            || matches!(
                type_expr,
                CsilTypeExpression::Literal(CsilLiteralValue::Null)
            )
    }

    /// Render a `@depends-on` condition tree into a single human-readable line
    /// for a field description. Compound nodes join with " and "/" or " and a
    /// nested compound is parenthesized so the grouping survives the flattening
    /// into one line.
    fn render_depends_condition(&self, condition: &CsilDependsCondition) -> String {
        match condition {
            CsilDependsCondition::Compare { field, op, value } => match (op, value) {
                (Some(op), Some(value)) => {
                    let op_str = match op {
                        CsilDependsCompareOp::Eq => "==",
                        CsilDependsCompareOp::Ne => "!=",
                        CsilDependsCompareOp::Lt => "<",
                        CsilDependsCompareOp::Le => "<=",
                        CsilDependsCompareOp::Gt => ">",
                        CsilDependsCompareOp::Ge => ">=",
                    };
                    format!("{field} {op_str} {}", self.render_depends_literal(value))
                }
                // No operator means a presence check (`@depends-on(field)`).
                _ => format!("{field} is present"),
            },
            CsilDependsCondition::All(conditions) => conditions
                .iter()
                .map(|c| self.render_depends_child(c))
                .collect::<Vec<_>>()
                .join(" and "),
            CsilDependsCondition::Any(conditions) => conditions
                .iter()
                .map(|c| self.render_depends_child(c))
                .collect::<Vec<_>>()
                .join(" or "),
        }
    }

    /// Parenthesize a child only when it is itself a compound (`All`/`Any`) so a
    /// mixed tree like `a and (b or c)` reads unambiguously while simple
    /// comparisons stay bare.
    fn render_depends_child(&self, condition: &CsilDependsCondition) -> String {
        match condition {
            CsilDependsCondition::Compare { .. } => self.render_depends_condition(condition),
            _ => format!("({})", self.render_depends_condition(condition)),
        }
    }

    fn render_depends_literal(&self, value: &CsilLiteralValue) -> String {
        match value {
            CsilLiteralValue::Text(s) => format!("'{s}'"),
            CsilLiteralValue::Integer(i) => i.to_string(),
            CsilLiteralValue::Float(f) => f.to_string(),
            CsilLiteralValue::Bool(b) => b.to_string(),
            CsilLiteralValue::Null => "null".to_string(),
            other => format!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{
        CsilControlOperator, CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry,
        CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilPosition,
        CsilRule, CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation,
        CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    };
    use std::collections::HashMap;

    fn create_test_config() -> GeneratorConfig {
        GeneratorConfig {
            target: "openapi".to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        }
    }

    fn create_test_position() -> CsilPosition {
        CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        }
    }

    /// Build a `CsilSpecSerialized` from rules, hiding the boilerplate of the
    /// non-rule fields (`source_content`, the counts) so test fixtures stay
    /// focused on what they're actually asserting.
    fn spec_from_rules(rules: Vec<CsilRule>) -> CsilSpecSerialized {
        let service_count = rules
            .iter()
            .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
            .count();
        CsilSpecSerialized {
            rules,
            source_content: None,
            service_count,
            fields_with_metadata_count: 0,
        }
    }

    #[test]
    fn test_generate_openapi_spec_with_basic_types() {
        let spec = spec_from_rules(vec![CsilRule {
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
                        key: Some(CsilGroupKey::Bare("age".to_string())),
                        value_type: CsilTypeExpression::Builtin("int".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, "openapi.json");

        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();
        assert_eq!(openapi_json["openapi"], "3.0.3");
        assert_eq!(openapi_json["info"]["title"], "Generated API");

        let user_schema = &openapi_json["components"]["schemas"]["User"];
        assert_eq!(user_schema["type"], "object");
        assert_eq!(user_schema["properties"]["name"]["type"], "string");
        assert_eq!(user_schema["properties"]["age"]["type"], "integer");

        let required = user_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("name")));
        assert!(!required.contains(&json!("age")));
    }

    #[test]
    fn test_generate_openapi_spec_with_service_operations() {
        let spec = spec_from_rules(vec![
            CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "UserService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![
                        CsilServiceOperation {
                            name: "create_user".to_string(),
                            input_type: CsilTypeExpression::Reference("User".to_string()),
                            output_type: CsilTypeExpression::Reference("User".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: None,
                        },
                        CsilServiceOperation {
                            name: "get_user".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Reference("User".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: None,
                        },
                    ],
                    wire_id: None,
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            },
        ]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let paths = &openapi_json["paths"];
        assert!(paths["/userservice/create_user"]["post"].is_object());
        assert!(paths["/userservice/get_user"]["post"].is_object());

        let create_user_op = &paths["/userservice/create_user"]["post"];
        assert_eq!(create_user_op["summary"], "create_user operation");
        assert_eq!(create_user_op["operationId"], "userservice_create_user");

        let request_schema =
            &create_user_op["requestBody"]["content"]["application/json"]["schema"];
        assert_eq!(request_schema["$ref"], "#/components/schemas/User");

        let response_schema =
            &create_user_op["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(response_schema["$ref"], "#/components/schemas/User");
    }

    #[test]
    fn test_generate_openapi_spec_with_field_metadata() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "User".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            CsilFieldMetadata::Description("User's full name".to_string()),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(2)),
                        ],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("email".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![
                            CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxLength(100)),
                        ],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("secret".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![CsilFieldMetadata::Visibility(
                            CsilFieldVisibility::ReceiveOnly,
                        )],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("age".to_string())),
                        value_type: CsilTypeExpression::Builtin("int".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![CsilFieldMetadata::DependsOn {
                            field: "status".to_string(),
                            value: Some(CsilLiteralValue::Text("verified".to_string())),
                        }],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let user_schema = &openapi_json["components"]["schemas"]["User"];

        let name_prop = &user_schema["properties"]["name"];
        assert_eq!(name_prop["description"], "User's full name");
        assert_eq!(name_prop["minLength"], 2);

        let email_prop = &user_schema["properties"]["email"];
        assert_eq!(
            email_prop["description"],
            "Send-only field (not included in responses)"
        );
        assert_eq!(email_prop["maxLength"], 100);

        let secret_prop = &user_schema["properties"]["secret"];
        assert_eq!(
            secret_prop["description"],
            "Receive-only field (not included in requests)"
        );

        let age_prop = &user_schema["properties"]["age"];
        assert!(
            age_prop["description"]
                .as_str()
                .unwrap()
                .contains("Depends on field 'status'")
        );
    }

    #[test]
    fn test_generate_openapi_spec_with_array_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "UserList".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                occurrence: Some(CsilOccurrence::Range {
                    min: Some(1),
                    max: Some(10),
                }),
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let user_list_schema = &openapi_json["components"]["schemas"]["UserList"];
        assert_eq!(user_list_schema["type"], "array");
        assert_eq!(user_list_schema["items"]["type"], "string");
        assert_eq!(user_list_schema["minItems"], 1);
        assert_eq!(user_list_schema["maxItems"], 10);
    }

    #[test]
    fn test_generate_openapi_spec_with_map_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "StringMap".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                occurrence: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let string_map_schema = &openapi_json["components"]["schemas"]["StringMap"];
        assert_eq!(string_map_schema["type"], "object");
        assert_eq!(string_map_schema["additionalProperties"]["type"], "integer");
    }

    #[test]
    fn test_generate_openapi_spec_with_choice_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "StringOrInt".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("int".to_string()),
            ]),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let choice_schema = &openapi_json["components"]["schemas"]["StringOrInt"];
        assert!(choice_schema["oneOf"].is_array());

        let choices = choice_schema["oneOf"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0]["type"], "string");
        assert_eq!(choices[1]["type"], "integer");
    }

    // A `.ge`/`.le` bound on a string-typed `decimal` or `timestamp` field
    // cannot ride as a numeric `minimum`/`maximum` (that would be an invalid
    // OpenAPI schema), so it must survive as a string-valued vendor extension
    // rather than being silently dropped.
    #[test]
    fn string_typed_comparison_bounds_use_vendor_extension() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "user".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("balance".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Builtin("decimal".to_string())),
                            constraints: vec![CsilControlOperator::GreaterEqual(
                                CsilLiteralValue::Text("0.00".to_string()),
                            )],
                        },
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("created_at".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Builtin(
                                "timestamp".to_string(),
                            )),
                            constraints: vec![CsilControlOperator::GreaterEqual(
                                CsilLiteralValue::Text("1970-01-01T00:00:00Z".to_string()),
                            )],
                        },
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let balance = &openapi_json["components"]["schemas"]["user"]["properties"]["balance"];
        assert_eq!(balance["type"], "string");
        assert_eq!(balance["format"], "decimal");
        assert_eq!(balance["x-csil-minimum"], "0.00");
        assert!(
            balance.get("minimum").is_none(),
            "a string bound must not become a numeric `minimum`: {balance}"
        );

        let created_at = &openapi_json["components"]["schemas"]["user"]["properties"]["created_at"];
        assert_eq!(created_at["type"], "string");
        assert_eq!(created_at["format"], "date-time");
        assert_eq!(created_at["x-csil-minimum"], "1970-01-01T00:00:00Z");
        assert!(created_at.get("minimum").is_none());
    }

    #[test]
    fn test_generate_openapi_spec_with_range_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Age".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Range {
                start: Some(0),
                end: Some(120),
                inclusive: true,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let age_schema = &openapi_json["components"]["schemas"]["Age"];
        assert_eq!(age_schema["type"], "integer");
        assert_eq!(age_schema["minimum"], 0);
        assert_eq!(age_schema["maximum"], 120);
    }

    #[test]
    fn test_generate_openapi_spec_with_literal_values() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Status".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Literal(CsilLiteralValue::Text(
                "active".to_string(),
            ))),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let status_schema = &openapi_json["components"]["schemas"]["Status"];
        assert_eq!(status_schema["type"], "string");
        let enum_values = status_schema["enum"].as_array().unwrap();
        assert_eq!(enum_values[0], "active");
    }

    #[test]
    fn test_generate_openapi_spec_with_custom_config() {
        let mut config = create_test_config();
        config
            .options
            .insert("title".to_string(), json!("My Custom API"));
        config.options.insert("version".to_string(), json!("2.0.0"));
        config
            .options
            .insert("description".to_string(), json!("A custom API description"));

        let spec = spec_from_rules(vec![]);

        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        assert_eq!(openapi_json["info"]["title"], "My Custom API");
        assert_eq!(openapi_json["info"]["version"], "2.0.0");
        assert_eq!(
            openapi_json["info"]["description"],
            "A custom API description"
        );
    }

    #[test]
    fn test_generate_openapi_spec_with_bidirectional_service() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "NotificationService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "subscribe".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Bidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let subscribe_op = &openapi_json["paths"]["/notificationservice/subscribe"]["post"];
        assert!(subscribe_op.is_object());
        assert_eq!(subscribe_op["operationId"], "notificationservice_subscribe");
    }

    #[test]
    fn test_generate_openapi_spec_with_reverse_service() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "CallbackService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "webhook".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Reverse,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let webhook_op = &openapi_json["paths"]["/callbackservice/webhook"]["get"];
        assert!(webhook_op.is_object());
    }

    #[test]
    fn unidirectional_null_input_op_omits_request_body() {
        // `op: -> Event` parses to a Unidirectional operation whose input is the
        // `null` builtin. Like the reverse push case, it must not emit a
        // `requestBody` (and never a `{"type":"null"}` body schema), since there
        // is no request payload for callers to send.
        let spec = spec_from_rules(vec![CsilRule {
            name: "PushService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "emit".to_string(),
                    input_type: CsilTypeExpression::Builtin("null".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let emit_op = &openapi_json["paths"]["/pushservice/emit"]["post"];
        assert!(emit_op.is_object());
        assert!(
            emit_op.get("requestBody").is_none(),
            "null-input unidirectional op must omit requestBody, got {emit_op:#}"
        );
        assert!(
            !result[0].content.contains("\"type\": \"null\""),
            "no operation body schema should be emitted for a null input"
        );
    }

    #[test]
    fn test_builtin_type_to_schema() {
        let generator = OpenApiGenerator::new(&create_test_config());

        assert_eq!(generator.builtin_type_to_schema("text")["type"], "string");
        assert_eq!(generator.builtin_type_to_schema("int")["type"], "integer");
        assert_eq!(generator.builtin_type_to_schema("float")["type"], "number");
        assert_eq!(generator.builtin_type_to_schema("bool")["type"], "boolean");
        assert_eq!(generator.builtin_type_to_schema("bytes")["type"], "string");
        assert_eq!(generator.builtin_type_to_schema("bytes")["format"], "byte");
    }

    #[test]
    fn timestamp_maps_to_date_time_string() {
        let generator = OpenApiGenerator::new(&create_test_config());
        let schema = generator.builtin_type_to_schema("timestamp");
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["format"], "date-time");
    }

    #[test]
    fn decimal_maps_to_decimal_string() {
        let generator = OpenApiGenerator::new(&create_test_config());
        let schema = generator.builtin_type_to_schema("decimal");
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["format"], "decimal");
    }

    /// Build a single-rule spec whose rule is a `TypeDef` of `type_expr`, then
    /// return the parsed schema generated for it. Keeps the constraint tests
    /// focused on the schema rather than spec boilerplate.
    fn schema_for_type(type_expr: CsilTypeExpression, config: &GeneratorConfig) -> Value {
        let spec = spec_from_rules(vec![CsilRule {
            name: "T".to_string(),
            rule_type: CsilRuleType::TypeDef(type_expr),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);
        let (result, _warnings) = generate_openapi_spec(&spec, config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();
        openapi_json["components"]["schemas"]["T"].clone()
    }

    fn constrained(
        base: CsilTypeExpression,
        constraints: Vec<CsilControlOperator>,
    ) -> CsilTypeExpression {
        CsilTypeExpression::Constrained {
            base_type: Box::new(base),
            constraints,
        }
    }

    #[test]
    fn timestamp_and_decimal_appear_in_generated_schema() {
        let config = create_test_config();
        let ts = schema_for_type(
            CsilTypeExpression::Builtin("timestamp".to_string()),
            &config,
        );
        assert_eq!(ts["type"], "string");
        assert_eq!(ts["format"], "date-time");

        let dec = schema_for_type(CsilTypeExpression::Builtin("decimal".to_string()), &config);
        assert_eq!(dec["type"], "string");
        assert_eq!(dec["format"], "decimal");
    }

    #[test]
    fn decimal_mapping_csil_and_library_are_accepted() {
        for value in ["csil", "library"] {
            let mut config = create_test_config();
            config
                .options
                .insert("decimal_mapping".to_string(), json!(value));
            // Both produce the identical schema — the option is a no-op for
            // OpenAPI but must not error.
            let dec = schema_for_type(CsilTypeExpression::Builtin("decimal".to_string()), &config);
            assert_eq!(dec["format"], "decimal");
        }
    }

    #[test]
    fn decimal_mapping_unknown_value_is_hard_error() {
        let mut config = create_test_config();
        config
            .options
            .insert("decimal_mapping".to_string(), json!("bignum"));
        let spec = spec_from_rules(vec![]);
        let err = generate_openapi_spec(&spec, &config).unwrap_err();
        assert!(format!("{err}").contains("decimal_mapping"));
    }

    #[test]
    fn decimal_mapping_non_string_value_is_hard_error() {
        let mut config = create_test_config();
        config
            .options
            .insert("decimal_mapping".to_string(), json!(42));
        let spec = spec_from_rules(vec![]);
        assert!(generate_openapi_spec(&spec, &config).is_err());
    }

    #[test]
    fn equal_operator_maps_to_const() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("text".to_string()),
                vec![CsilControlOperator::Equal(CsilLiteralValue::Text(
                    "fixed".to_string(),
                ))],
            ),
            &create_test_config(),
        );
        assert_eq!(schema["const"], "fixed");
    }

    #[test]
    fn not_equal_operator_maps_to_not_const() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("int".to_string()),
                vec![CsilControlOperator::NotEqual(CsilLiteralValue::Integer(0))],
            ),
            &create_test_config(),
        );
        assert_eq!(schema["not"]["const"], 0);
    }

    #[test]
    fn bits_operator_maps_to_extension() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("uint".to_string()),
                vec![CsilControlOperator::Bits("flags".to_string())],
            ),
            &create_test_config(),
        );
        assert_eq!(schema["x-csil-bits"], "flags");
    }

    #[test]
    fn and_operator_maps_to_all_of() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("text".to_string()),
                vec![CsilControlOperator::And(Box::new(
                    CsilTypeExpression::Reference("Other".to_string()),
                ))],
            ),
            &create_test_config(),
        );
        let all_of = schema["allOf"].as_array().unwrap();
        assert_eq!(all_of.len(), 2);
        assert_eq!(all_of[0]["type"], "string");
        assert_eq!(all_of[1]["$ref"], "#/components/schemas/Other");
    }

    #[test]
    fn repeated_and_stays_flat_all_of() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("text".to_string()),
                vec![
                    CsilControlOperator::And(Box::new(CsilTypeExpression::Reference(
                        "A".to_string(),
                    ))),
                    CsilControlOperator::And(Box::new(CsilTypeExpression::Reference(
                        "B".to_string(),
                    ))),
                ],
            ),
            &create_test_config(),
        );
        let all_of = schema["allOf"].as_array().unwrap();
        assert_eq!(all_of.len(), 3);
        assert_eq!(all_of[1]["$ref"], "#/components/schemas/A");
        assert_eq!(all_of[2]["$ref"], "#/components/schemas/B");
    }

    #[test]
    fn within_operator_maps_to_extension() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("int".to_string()),
                vec![CsilControlOperator::Within(Box::new(
                    CsilTypeExpression::Reference("AllowedSet".to_string()),
                ))],
            ),
            &create_test_config(),
        );
        assert_eq!(
            schema["x-csil-within"]["$ref"],
            "#/components/schemas/AllowedSet"
        );
    }

    // The inner type of `.within`/`.and` must be run through the real
    // type-expression converter, never `format!("{expr:?}")`. A Debug-format
    // regression would emit a string like `Reference("X")` instead of a schema
    // object, so assert both that the membership/intersection carry a real
    // `$ref` schema object and that no Debug blob leaks into the output.
    fn assert_no_debug_blob(schema: &Value) {
        let serialized = schema.to_string();
        assert!(
            !serialized.contains("Reference("),
            "schema leaked a Rust Debug blob instead of a real schema: {serialized}"
        );
    }

    #[test]
    fn within_emits_real_schema_not_debug_blob() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("int".to_string()),
                vec![CsilControlOperator::Within(Box::new(
                    CsilTypeExpression::Reference("SomeType".to_string()),
                ))],
            ),
            &create_test_config(),
        );
        assert!(
            schema["x-csil-within"].is_object(),
            "x-csil-within must be a schema object, got {}",
            schema["x-csil-within"]
        );
        assert_eq!(
            schema["x-csil-within"]["$ref"],
            "#/components/schemas/SomeType"
        );
        assert_no_debug_blob(&schema);
    }

    #[test]
    fn and_emits_real_schema_not_debug_blob() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("text".to_string()),
                vec![CsilControlOperator::And(Box::new(
                    CsilTypeExpression::Reference("SomeType".to_string()),
                ))],
            ),
            &create_test_config(),
        );
        let all_of = schema["allOf"].as_array().unwrap();
        let member = all_of
            .iter()
            .find(|m| m.get("$ref").is_some())
            .expect("an allOf member must carry the intersected type as a real $ref schema");
        assert_eq!(member["$ref"], "#/components/schemas/SomeType");
        assert_no_debug_blob(&schema);
    }

    // A `.ge`/`.le` bound on a string-typed `decimal` can be an Integer literal
    // (the core guarantees no Float on decimal). It must render as its decimal
    // string in the vendor extension, exactly like a text bound such as "0.00",
    // never as a JSON number that would be invalid alongside a string instance.
    #[test]
    fn integer_decimal_bound_renders_as_decimal_string() {
        let schema = schema_for_type(
            constrained(
                CsilTypeExpression::Builtin("decimal".to_string()),
                vec![
                    CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(0)),
                    CsilControlOperator::LessEqual(CsilLiteralValue::Integer(100)),
                ],
            ),
            &create_test_config(),
        );
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["format"], "decimal");
        assert_eq!(schema["x-csil-minimum"], "0");
        assert_eq!(schema["x-csil-maximum"], "100");
        assert!(
            schema.get("minimum").is_none() && schema.get("maximum").is_none(),
            "an integer bound on a string-typed decimal must not become a numeric keyword: {schema}"
        );
    }

    #[test]
    fn test_generate_openapi_spec_validates_json() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "ComplexType".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("nested".to_string())),
                    value_type: CsilTypeExpression::Array {
                        element_type: Box::new(CsilTypeExpression::Map {
                            key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            value: Box::new(CsilTypeExpression::Choice(vec![
                                CsilTypeExpression::Builtin("int".to_string()),
                                CsilTypeExpression::Builtin("text".to_string()),
                            ])),
                            occurrence: None,
                        }),
                        occurrence: Some(CsilOccurrence::OneOrMore),
                    },
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();

        let parsed_json: Value =
            serde_json::from_str(&result[0].content).expect("Generated JSON should be valid");
        assert_eq!(parsed_json["openapi"], "3.0.3");
    }

    #[test]
    fn bidirectional_op_emits_x_csil_direction_and_warning() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Chat".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "subscribe".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Bidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // Bidirectional maps to POST as a documentation placeholder, but the
        // operation carries `x-csil-direction: "bidirectional"` so consumers
        // can detect that HTTP is a stand-in.
        let op = &openapi["paths"]["/chat/subscribe"]["post"];
        assert_eq!(op["x-csil-direction"], "bidirectional");

        // A warning explains the limitation.
        assert_eq!(warnings.len(), 1);
        let msg = &warnings[0].message;
        assert!(msg.contains("Chat.subscribe"));
        assert!(msg.contains("bidirectional"));
        assert!(msg.contains("<->"));
    }

    #[test]
    fn reverse_op_emits_x_csil_direction_and_warning() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Notify".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "deleted".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Reverse,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // Reverse maps to GET (server-pushed semantics shoe-horned into HTTP).
        let op = &openapi["paths"]["/notify/deleted"]["get"];
        assert_eq!(op["x-csil-direction"], "reverse");

        assert_eq!(warnings.len(), 1);
        let msg = &warnings[0].message;
        assert!(msg.contains("Notify.deleted"));
        assert!(msg.contains("reverse"));
        assert!(msg.contains("<-"));
    }

    #[test]
    fn unidirectional_op_does_not_emit_x_csil_direction_or_warning() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Auth".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "login".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // `->` is a faithful HTTP POST; no extension, no warning.
        let op = &openapi["paths"]["/auth/login"]["post"];
        assert!(op.get("x-csil-direction").is_none());
        assert!(warnings.is_empty());
    }

    /// Build a tuple group expression from a list of value types, each as a
    /// bare-keyed entry (keys are irrelevant to the array schema).
    fn tuple_of(value_types: Vec<CsilTypeExpression>) -> CsilTypeExpression {
        let entries = value_types
            .into_iter()
            .enumerate()
            .map(|(i, value_type)| CsilGroupEntry {
                key: Some(CsilGroupKey::Bare(format!("e{i}"))),
                value_type,
                occurrence: None,
                metadata: Vec::new(),
                doc_comments: Vec::new(),
            })
            .collect();
        CsilTypeExpression::Tuple(CsilGroupExpression { entries })
    }

    #[test]
    fn tuple_emits_length_pinned_array_with_oneof_items() {
        let schema = schema_for_type(
            tuple_of(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("int".to_string()),
                CsilTypeExpression::Builtin("bool".to_string()),
            ]),
            &create_test_config(),
        );
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["minItems"], 3);
        assert_eq!(schema["maxItems"], 3);
        let one_of = schema["items"]["oneOf"].as_array().unwrap();
        // Three distinct value types produce three union branches.
        assert_eq!(one_of.len(), 3);
        assert!(one_of.iter().any(|s| s["type"] == "string"));
        assert!(one_of.iter().any(|s| s["type"] == "integer"));
        assert!(one_of.iter().any(|s| s["type"] == "boolean"));
    }

    #[test]
    fn tuple_collapses_duplicate_entry_types_to_single_items_schema() {
        let schema = schema_for_type(
            tuple_of(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("text".to_string()),
            ]),
            &create_test_config(),
        );
        assert_eq!(schema["minItems"], 2);
        assert_eq!(schema["maxItems"], 2);
        // One distinct type means no pointless single-branch `oneOf`.
        assert!(schema["items"].get("oneOf").is_none());
        assert_eq!(schema["items"]["type"], "string");
    }

    #[test]
    fn empty_tuple_emits_zero_length_array() {
        let schema = schema_for_type(tuple_of(vec![]), &create_test_config());
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["minItems"], 0);
        assert_eq!(schema["maxItems"], 0);
        // An empty tuple's items schema is unconstrained, never an invalid
        // empty `oneOf`.
        assert!(schema["items"].get("oneOf").is_none());
    }

    #[test]
    fn keyed_tuple_uses_entry_value_types() {
        // `[tag: text, value: any]` — keys don't affect the array schema, only
        // the value types do.
        let schema = schema_for_type(
            tuple_of(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("any".to_string()),
            ]),
            &create_test_config(),
        );
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["minItems"], 2);
        let one_of = schema["items"]["oneOf"].as_array().unwrap();
        // `any` is the empty schema, distinct from the `text` branch.
        assert_eq!(one_of.len(), 2);
        assert!(one_of.iter().any(|s| s["type"] == "string"));
    }

    fn field_with_depends_expr(condition: CsilDependsCondition) -> Value {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Form".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("field".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![CsilFieldMetadata::DependsOnExpr(condition)],
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);
        let (result, _warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();
        openapi["components"]["schemas"]["Form"]["properties"]["field"].clone()
    }

    #[test]
    fn depends_on_expr_compare_renders_in_description() {
        let field = field_with_depends_expr(CsilDependsCondition::Compare {
            field: "status".to_string(),
            op: Some(CsilDependsCompareOp::Eq),
            value: Some(CsilLiteralValue::Text("active".to_string())),
        });
        assert_eq!(field["description"], "Depends on: status == 'active'");
    }

    #[test]
    fn depends_on_expr_presence_renders_in_description() {
        let field = field_with_depends_expr(CsilDependsCondition::Compare {
            field: "token".to_string(),
            op: None,
            value: None,
        });
        assert_eq!(field["description"], "Depends on: token is present");
    }

    #[test]
    fn depends_on_expr_all_and_any_render_readable_tree() {
        let field = field_with_depends_expr(CsilDependsCondition::All(vec![
            CsilDependsCondition::Compare {
                field: "kind".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("paid".to_string())),
            },
            CsilDependsCondition::Any(vec![
                CsilDependsCondition::Compare {
                    field: "amount".to_string(),
                    op: Some(CsilDependsCompareOp::Gt),
                    value: Some(CsilLiteralValue::Integer(0)),
                },
                CsilDependsCondition::Compare {
                    field: "waived".to_string(),
                    op: None,
                    value: None,
                },
            ]),
        ]));
        // " and "/" or " joiners with the nested compound parenthesized.
        assert_eq!(
            field["description"],
            "Depends on: kind == 'paid' and (amount > 0 or waived is present)"
        );
    }

    #[test]
    fn push_only_null_input_op_omits_request_body() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Events".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "ping".to_string(),
                    input_type: CsilTypeExpression::Builtin("null".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Reverse,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, _warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // A `null`-input (push-only) op carries no request body rather than a
        // required `null`-typed one, yet still describes its response.
        let op = &openapi["paths"]["/events/ping"]["get"];
        assert!(op.is_object());
        assert!(op.get("requestBody").is_none());
        assert!(op["responses"]["200"].is_object());
    }

    #[test]
    fn non_null_input_op_still_has_required_request_body() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Auth".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "login".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, _warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();
        let op = &openapi["paths"]["/auth/login"]["post"];
        assert_eq!(op["requestBody"]["required"], true);
        assert_eq!(
            op["requestBody"]["content"]["application/json"]["schema"]["type"],
            "string"
        );
    }

    fn wire_id_spec(service_wire: Option<u64>, op_wire: Option<u64>) -> CsilSpecSerialized {
        spec_from_rules(vec![CsilRule {
            name: "OrderService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "place_order".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: op_wire,
                }],
                wire_id: service_wire,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }])
    }

    #[test]
    fn wire_ids_emitted_as_vendor_extension_when_present() {
        let spec = wire_id_spec(Some(3), Some(7));
        let (result, _warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // Operation ordinal on the per-operation object.
        let op = &openapi["paths"]["/orderservice/place_order"]["post"];
        assert_eq!(op["x-csil-wire-id"], json!(7));

        // Service ordinal on the service-level object (an OpenAPI tag).
        let tags = openapi["tags"].as_array().expect("tags array");
        let tag = tags
            .iter()
            .find(|t| t["name"] == json!("OrderService"))
            .expect("OrderService tag");
        assert_eq!(tag["x-csil-wire-id"], json!(3));
    }

    #[test]
    fn wire_ids_absent_when_unset() {
        let spec = wire_id_spec(None, None);
        let (result, _warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        assert!(
            !result[0].content.contains("x-csil-wire-id"),
            "no wire-id extension when service has no wire-id"
        );
        assert!(
            !result[0].content.contains("\"tags\""),
            "no tags emitted when no service carries a wire-id"
        );
    }
}
