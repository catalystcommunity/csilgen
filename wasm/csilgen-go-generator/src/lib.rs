//! Go Code Generator for CSIL
//!
//! This example generator demonstrates how to create a fully functional
//! CSIL generator that produces Go code with struct definitions and service interfaces.

use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilFieldVisibility, CsilGroupEntry, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSizeConstraint,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
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
    let config = GoConfig::from_options(&input.config.options)?;
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

    // The exact-decimal helper is self-contained and only worth emitting when the
    // spec actually uses `decimal` under the default (`csil`) mapping; the library
    // mapping pulls the type from shopspring instead, so no helper is generated.
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(&input, "decimal") {
        files.push(GeneratedFile {
            path: make_path("csil_decimal.gen.go"),
            content: csil_decimal_file(&config),
        });
    }

    // Dispatch on target: the base `go` (and explicit `go-server`) target emits
    // the server interface; `go-client` emits a transport-agnostic client;
    // `go-typesonly` emits the types (and their validation/constructors) alone.
    // An unrecognized sub-target is an error, not a silent fall-through.
    enum Surface {
        Server,
        Client,
        TypesOnly,
    }
    let surface = match input.config.target.as_str() {
        "go" | "go-server" => Surface::Server,
        "go-client" => Surface::Client,
        "go-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client_content) = generate_client(&input, &config, &mut warnings)? {
                    files.push(GeneratedFile {
                        path: make_path("client.gen.go"),
                        content: client_content,
                    });
                }
            }
            Surface::Server => {
                if let Some(services_content) = generate_services(&input, &config, &mut warnings)? {
                    files.push(GeneratedFile {
                        path: make_path("services.gen.go"),
                        content: services_content,
                    });
                }
            }
            Surface::TypesOnly => {}
        }
    }

    // Generate validation file if there are constraints. Constraints arrive via
    // two parallel systems — `@`-annotations (counted in fields_with_metadata_count)
    // and `.`-control-operators carried inline on the field's type — so the gate
    // alone is not authoritative; generate_validation returns None when neither
    // surface actually yields a check.
    let validation_content = generate_validation(&input, &config, &mut warnings)?;
    // The timestamp must-parser is defined in the validation file when a timestamp
    // comparison lands there; the constructor file then references it rather than
    // re-declaring it (one definition per package).
    let timestamp_helper_defined = validation_content
        .as_deref()
        .is_some_and(|c| c.contains("func mustParseTimestamp"));
    if let Some(validation_content) = validation_content {
        files.push(GeneratedFile {
            path: make_path("validation.gen.go"),
            content: validation_content,
        });
    }

    // Generate constructors file if there are types with defaults
    if config.generate_constructors
        && let Some(constructors_content) =
            generate_constructors(&input, &config, &mut warnings, timestamp_helper_defined)?
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

/// In-memory Go type selected for the CSIL `decimal` core type. The wire form is
/// CBOR tag 4 either way; this only changes what the generated struct field is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` helper (no third-party dependency).
    Csil,
    /// Use `github.com/shopspring/decimal.Decimal`.
    Library,
}

#[derive(Debug)]
struct GoConfig {
    package_name: String,
    output_subdir: String,
    use_json_tags: bool,
    use_yaml_tags: bool,
    use_cbor_tags: bool,
    generate_validation: bool,
    generate_constructors: bool,
    decimal_mapping: DecimalMapping,
    indent_style: String,
    go_imports: Vec<String>,
}

impl GoConfig {
    /// The Go type a `decimal` field maps to under the active mapping. Both forms
    /// carry the identical CBOR tag-4 wire value; only the in-memory type differs.
    fn decimal_go_type(&self) -> &'static str {
        match self.decimal_mapping {
            DecimalMapping::Csil => "CsilDecimal",
            DecimalMapping::Library => "decimal.Decimal",
        }
    }

    /// Parse options into a config. A `decimal_mapping` other than `"csil"`
    /// (default) or `"library"` is a hard error so misconfiguration surfaces at
    /// generation time rather than silently degrading, matching the validate-early
    /// idiom used for `ts_bidirectional_transport`.
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Result<Self, i32> {
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

        let decimal_mapping = match options.get("decimal_mapping") {
            None => DecimalMapping::Csil,
            Some(v) => match v.as_str() {
                Some("csil") => DecimalMapping::Csil,
                Some("library") => DecimalMapping::Library,
                _ => return Err(error_codes::GENERATION_ERROR),
            },
        };

        Ok(Self {
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
            // CSIL is the CBOR Service Interface Language: the canonical wire is
            // CBOR keyed by the CSIL field name verbatim. fxamacker/cbor keys by
            // the Go field name (PascalCase) unless a `cbor` tag says otherwise,
            // so these tags are on by default to keep four-language clients aligned.
            use_cbor_tags: options
                .get("use_cbor_tags")
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
            decimal_mapping,
            indent_style: "\t".to_string(), // Go convention is tabs
            go_imports,
        })
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
            content.push_str(&format!("// {line}\n"));
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

    // Imports are the caller-configured set plus whatever the mapped types force:
    // `timestamp` needs `time`, and a `decimal` under the library mapping needs
    // shopspring. The default decimal mapping pulls no third-party package here —
    // its CsilDecimal lives in the same package, in its own generated file.
    let mut imports = config.go_imports.clone();
    if spec_uses_builtin(input, "timestamp") {
        imports.push("time".to_string());
    }
    if config.decimal_mapping == DecimalMapping::Library && spec_uses_builtin(input, "decimal") {
        imports.push("github.com/shopspring/decimal".to_string());
    }
    if !imports.is_empty() {
        content.push_str("import (\n");
        for import_path in &imports {
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
                            map_csil_type_to_go(
                                &entry.value_type,
                                &entry.occurrence,
                                config.decimal_go_type(),
                            )
                        });

                        // Add field documentation
                        if let Some(description) = get_field_description(&entry.metadata) {
                            content
                                .push_str(&format!("{}// {}\n", config.indent_style, description));
                        }

                        if let Some(depends) = get_depends_comment(&entry.metadata) {
                            content.push_str(&format!(
                                "{}// depends-on: {depends}\n",
                                config.indent_style
                            ));
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
                                        message: format!("Field '{field_name}' marked as send-only, consider separate request/response types"),
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

                        // Canonical CBOR wire key: the CSIL field name verbatim,
                        // so a Go server and Rust/Python/TS clients agree on map keys.
                        if config.use_cbor_tags {
                            let cbor_name = go_json_name_from_key(key);
                            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                tag_parts.push(format!("cbor:\"{cbor_name},omitempty\""));
                            } else {
                                tag_parts.push(format!("cbor:\"{cbor_name}\""));
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
                                    map_csil_type_to_go(
                                        &entry.value_type,
                                        &entry.occurrence,
                                        config.decimal_go_type(),
                                    )
                                });

                            if let Some(description) = get_field_description(&entry.metadata) {
                                content.push_str(&format!(
                                    "{}// {}\n",
                                    config.indent_style, description
                                ));
                            }

                            if let Some(depends) = get_depends_comment(&entry.metadata) {
                                content.push_str(&format!(
                                    "{}// depends-on: {depends}\n",
                                    config.indent_style
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

                            if config.use_cbor_tags {
                                let cbor_name = go_json_name_from_key(key);
                                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                    tag_parts.push(format!("cbor:\"{cbor_name},omitempty\""));
                                } else {
                                    tag_parts.push(format!("cbor:\"{cbor_name}\""));
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
                    let go_type = map_csil_type_to_go(type_expr, &None, config.decimal_go_type());
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

/// Client scaffolding emitted once at the top of `client.gen.go`: the error type
/// and the caller-supplied `Transport` every per-service client delegates to.
const CLIENT_PRELUDE_GO: &str = "\
// ClientError is returned by a generated client call: a structured error the
// service returned (Code/Message), or a transport-level failure (Err).
type ClientError struct {
\tCode    int64
\tMessage string
\tErr     error
}

func (e *ClientError) Error() string {
\tif e.Err != nil {
\t\treturn \"transport error: \" + e.Err.Error()
\t}
\treturn fmt.Sprintf(\"service error %d: %s\", e.Code, e.Message)
}

// Transport is supplied by the caller: it encodes req (CBOR over HTTP, say),
// performs the call named by (service, method), and decodes the response into
// resp, or returns an error. The generator never owns the wire.
type Transport interface {
\tCall(ctx context.Context, service string, method string, req any, resp any) error
}
";

/// The body of the self-contained `CsilDecimal` helper, injected as its own file
/// only when the spec uses `decimal` under the default mapping. It holds the exact
/// value (int exponent + big.Int mantissa) and (de)serializes as CBOR tag 4
/// `[exponent, mantissa]`. Conversion to/from shopspring is via String() and
/// ParseCsilDecimal, so the helper itself takes no dependency on shopspring.
const CSIL_DECIMAL_GO: &str = r#"// CsilDecimal is the exact, base-10 `decimal` core type. On the wire it is CBOR
// tag 4 (decimal fraction): a two-element array [exponent, mantissa] whose value
// is Mantissa * 10^Exponent. The value is kept as exact integers, never a float,
// so no precision is lost. Interop with github.com/shopspring/decimal is via
// String()/ParseCsilDecimal, so this type needs no dependency on it.
type CsilDecimal struct {
	Exponent int64
	Mantissa *big.Int
}

// mantissa treats the zero value as 0 so a never-assigned CsilDecimal is usable.
func (d CsilDecimal) mantissa() *big.Int {
	if d.Mantissa == nil {
		return big.NewInt(0)
	}
	return d.Mantissa
}

// MarshalCBOR encodes the value as CBOR tag 4: [exponent, mantissa].
func (d CsilDecimal) MarshalCBOR() ([]byte, error) {
	return cbor.Marshal(cbor.Tag{
		Number:  4,
		Content: []interface{}{d.Exponent, d.mantissa()},
	})
}

// UnmarshalCBOR decodes a CBOR tag 4 decimal fraction. The mantissa may arrive as
// a fixed-width integer or a bignum depending on its magnitude; both are exact.
func (d *CsilDecimal) UnmarshalCBOR(data []byte) error {
	var tag cbor.Tag
	if err := cbor.Unmarshal(data, &tag); err != nil {
		return err
	}
	if tag.Number != 4 {
		return fmt.Errorf("CsilDecimal: expected CBOR tag 4, got %d", tag.Number)
	}
	arr, ok := tag.Content.([]interface{})
	if !ok || len(arr) != 2 {
		return fmt.Errorf("CsilDecimal: tag 4 content must be a two-element array")
	}
	exp, err := csilDecimalToInt64(arr[0])
	if err != nil {
		return fmt.Errorf("CsilDecimal: exponent: %w", err)
	}
	mant, err := csilDecimalToBigInt(arr[1])
	if err != nil {
		return fmt.Errorf("CsilDecimal: mantissa: %w", err)
	}
	d.Exponent = exp
	d.Mantissa = mant
	return nil
}

// String renders the exact value as canonical decimal text. This is the lossless
// bridge to other decimal libraries, e.g. shopspring.NewFromString(d.String()).
func (d CsilDecimal) String() string {
	m := d.mantissa()
	if d.Exponent == 0 {
		return m.String()
	}
	if d.Exponent > 0 {
		scale := new(big.Int).Exp(big.NewInt(10), big.NewInt(d.Exponent), nil)
		return new(big.Int).Mul(m, scale).String()
	}
	neg := m.Sign() < 0
	digits := new(big.Int).Abs(m).String()
	scale := int(-d.Exponent)
	sign := ""
	if neg {
		sign = "-"
	}
	if len(digits) <= scale {
		return sign + "0." + strings.Repeat("0", scale-len(digits)) + digits
	}
	return sign + digits[:len(digits)-scale] + "." + digits[len(digits)-scale:]
}

// ParseCsilDecimal parses canonical decimal text (what String produces, and what
// shopspring.Decimal.String emits) into an exact CsilDecimal.
func ParseCsilDecimal(s string) (CsilDecimal, error) {
	s = strings.TrimSpace(s)
	neg := false
	switch {
	case strings.HasPrefix(s, "-"):
		neg = true
		s = s[1:]
	case strings.HasPrefix(s, "+"):
		s = s[1:]
	}
	intPart, fracPart := s, ""
	if i := strings.IndexByte(s, '.'); i >= 0 {
		intPart, fracPart = s[:i], s[i+1:]
	}
	digits := intPart + fracPart
	if digits == "" {
		digits = "0"
	}
	m, ok := new(big.Int).SetString(digits, 10)
	if !ok {
		return CsilDecimal{}, fmt.Errorf("CsilDecimal: invalid decimal string %q", s)
	}
	if neg {
		m.Neg(m)
	}
	return CsilDecimal{Exponent: -int64(len(fracPart)), Mantissa: m}, nil
}

// mustParseCsilDecimal parses a bound literal embedded at generation time. The text
// is fixed by the spec, so a parse failure signals a generator bug, not bad input.
func mustParseCsilDecimal(s string) CsilDecimal {
	d, err := ParseCsilDecimal(s)
	if err != nil {
		panic(err)
	}
	return d
}

// Cmp returns -1, 0, or +1 as d is less than, equal to, or greater than other.
// The comparison is exact: both values are scaled to a common exponent and their
// integer mantissas compared, so no float rounding can flip the result.
func (d CsilDecimal) Cmp(other CsilDecimal) int {
	dm := new(big.Int).Set(d.mantissa())
	om := new(big.Int).Set(other.mantissa())
	switch {
	case d.Exponent > other.Exponent:
		scale := new(big.Int).Exp(big.NewInt(10), big.NewInt(d.Exponent-other.Exponent), nil)
		dm.Mul(dm, scale)
	case other.Exponent > d.Exponent:
		scale := new(big.Int).Exp(big.NewInt(10), big.NewInt(other.Exponent-d.Exponent), nil)
		om.Mul(om, scale)
	}
	return dm.Cmp(om)
}

func csilDecimalToInt64(v interface{}) (int64, error) {
	switch n := v.(type) {
	case int64:
		return n, nil
	case uint64:
		return int64(n), nil
	case int:
		return int64(n), nil
	case big.Int:
		return n.Int64(), nil
	case *big.Int:
		return n.Int64(), nil
	default:
		return 0, fmt.Errorf("expected integer, got %T", v)
	}
}

func csilDecimalToBigInt(v interface{}) (*big.Int, error) {
	switch n := v.(type) {
	case int64:
		return big.NewInt(n), nil
	case uint64:
		return new(big.Int).SetUint64(n), nil
	case int:
		return big.NewInt(int64(n)), nil
	case big.Int:
		return new(big.Int).Set(&n), nil
	case *big.Int:
		return new(big.Int).Set(n), nil
	default:
		return nil, fmt.Errorf("expected integer, got %T", v)
	}
}
"#;

/// Assemble the standalone `csil_decimal.gen.go` file: package header, the imports
/// the helper needs (`math/big`, `strings`, the cbor codec), and the helper body.
fn csil_decimal_file(config: &GoConfig) -> String {
    let mut content = String::new();
    content.push_str(&format!(
        "// Package {} contains the exact-decimal helper.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));
    content.push_str("import (\n");
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    content.push_str(&format!("{}\"math/big\"\n", config.indent_style));
    content.push_str(&format!("{}\"strings\"\n", config.indent_style));
    content.push('\n');
    content.push_str(&format!(
        "{}\"github.com/fxamacker/cbor/v2\"\n",
        config.indent_style
    ));
    content.push_str(")\n\n");
    content.push_str(CSIL_DECIMAL_GO);
    content
}

/// Strip a trailing `Service` suffix and PascalCase the remainder, matching the
/// wire service base used across the TypeScript/Rust/Python clients.
fn go_service_base(name: &str) -> String {
    let pascal = pascal_case(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

fn generate_client(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    let mut content = String::new();

    content.push_str(&format!(
        "// Package {} contains generated service clients.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    content.push_str("import (\n");
    content.push_str(&format!("{}\"context\"\n", config.indent_style));
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    content.push_str(")\n\n");

    content.push_str(CLIENT_PRELUDE_GO);
    content.push('\n');

    let mut emitted_any = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_struct(&mut content, &rule.name, service, config);
            emitted_any = true;
        }
    }

    if emitted_any {
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

fn emit_client_struct(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    let base = go_service_base(name);
    let client = format!("{base}Client");
    let wire_service = base.to_lowercase();

    content.push_str(&format!(
        "// {client} is a typed client for the {name} service.\n"
    ));
    content.push_str(&format!("type {client} struct {{\n"));
    content.push_str(&format!("{}transport Transport\n", config.indent_style));
    content.push_str("}\n\n");

    content.push_str(&format!(
        "func New{client}(transport Transport) *{client} {{\n"
    ));
    content.push_str(&format!(
        "{}return &{client}{{transport: transport}}\n",
        config.indent_style
    ));
    content.push_str("}\n\n");

    for operation in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops
        // ride the router/encoder surface emitted by the base `go` target.
        if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
            content.push_str(&format!(
                "// channel operation {} is not part of the RPC client\n\n",
                operation.name
            ));
            continue;
        }
        let method_name = go_method_name(&operation.name);
        let output_type = map_csil_type_to_go(
            &go_success_type(&operation.output_type),
            &None,
            config.decimal_go_type(),
        );
        let null_input = op_input_is_null(&operation.input_type);
        let params = if null_input {
            "ctx context.Context".to_string()
        } else {
            let input_type =
                map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
            format!("ctx context.Context, req {input_type}")
        };
        // With no request parameter there is nothing to marshal; the transport
        // still needs a payload argument, so pass an explicit nil.
        let req_arg = if null_input { "nil" } else { "req" };
        content.push_str(&format!(
            "func (c *{client}) {method_name}({params}) ({output_type}, error) {{\n"
        ));
        content.push_str(&format!("{}var resp {output_type}\n", config.indent_style));
        content.push_str(&format!(
            "{}err := c.transport.Call(ctx, \"{wire_service}\", \"{method_name}\", {req_arg}, &resp)\n",
            config.indent_style
        ));
        content.push_str(&format!("{}return resp, err\n", config.indent_style));
        content.push_str("}\n\n");
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
            emit_wire_ids(&mut content, &rule.name, service);

            if service_has_channel_ops(service) {
                emit_channel_router(&mut content, &rule.name, service, config);
                // Compact-profile twin, emitted only for wire-id-bearing services
                // so wire-id-free specs stay byte-identical.
                emit_channel_router_compact(&mut content, &rule.name, service, config);
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
                let output_type = map_csil_type_to_go(
                    &go_success_type(&operation.output_type),
                    &None,
                    config.decimal_go_type(),
                );
                let params = if op_input_is_null(&operation.input_type) {
                    "ctx context.Context".to_string()
                } else {
                    let input_type =
                        map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
                    format!("ctx context.Context, req {input_type}")
                };
                content.push_str(&format!(
                    "{}{method_name}({params}) ({output_type}, error)\n",
                    config.indent_style
                ));
            }
            CsilServiceDirection::Bidirectional => {
                // Fire-and-forget inbound: the implementer's plumbing pulls a
                // frame off the wire, hands it to Route<Service>Channel, which
                // decodes and dispatches here.
                let input_type =
                    map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
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

/// Emit `const` wire-id ordinals exposing the `@wire-id(N)` values so a host can
/// reference them instead of hardcoding. Purely additive: emits nothing unless
/// the service carries a wire-id, keeping wire-id-free output byte-identical.
fn emit_wire_ids(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let prefix = pascal_case(name);
    content.push_str(&format!(
        "// Wire-id ordinals for the {name} service (transport compact profiles).\n"
    ));
    content.push_str(&format!(
        "const {prefix}ServiceWireID uint64 = {service_id}\n"
    ));
    for operation in &service.operations {
        if let Some(op_id) = operation.wire_id {
            // The `Op` infix keeps operation ordinals distinct from the service
            // ordinal: an op named `service` emits `<Service>OpServiceWireID`,
            // never `<Service>ServiceWireID`, so the two can't redeclare a name.
            let op_exported = go_method_name(&operation.name);
            content.push_str(&format!(
                "const {prefix}Op{op_exported}WireID uint64 = {op_id}\n"
            ));
        }
    }
    content.push('\n');
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
        let input_type =
            map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
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

/// The compact-profile twin of `emit_channel_router`: when the service carries
/// `@wire-id` ordinals, emit `Route<Service>ChannelCompact` that dispatches on
/// the operation ordinal (`uint64`) instead of the wire method name. The profile
/// is negotiated on the wire (never declared in CSIL), so a host keeps both
/// routers and calls whichever the peer selected. Emits nothing for wire-id-free
/// services, keeping their output byte-identical.
fn emit_channel_router_compact(
    content: &mut String,
    service_name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    if service.wire_id.is_none() {
        return;
    }
    let route_fn = format!("Route{service_name}ChannelCompact");
    content.push_str(&format!(
        "// {route_fn} decodes one inbound channel frame by its @wire-id ordinal\n\
         // (compact transport profile) and dispatches to the matching\n\
         // {service_name} method. The verbose-profile twin is Route{service_name}Channel;\n\
         // the host calls whichever matches the profile negotiated on the wire.\n"
    ));
    content.push_str(&format!(
        "func {route_fn}(handlers {service_name}, ctx context.Context, codec Codec, op uint64, data []byte) error {{\n"
    ));
    content.push_str(&format!("{}switch op {{\n", config.indent_style));
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        // The all-or-nothing wire-id rule (enforced by the validator) means a
        // bidirectional op on a wire-id-bearing service always has an ordinal.
        let Some(op_id) = operation.wire_id else {
            continue;
        };
        let method_name = go_method_name(&operation.name);
        let input_type =
            map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
        content.push_str(&format!("{}case {op_id}:\n", config.indent_style));
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
        "{}{}return fmt.Errorf(\"unknown channel ordinal %d\", op)\n",
        config.indent_style, config.indent_style
    ));
    content.push_str(&format!("{}}}\n", config.indent_style));
    content.push_str("}\n\n");
}

fn emit_channel_encoders(
    content: &mut String,
    service_name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    for operation in &service.operations {
        if !matches!(
            operation.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method_name = go_method_name(&operation.name);
        let output_type =
            map_csil_type_to_go(&operation.output_type, &None, config.decimal_go_type());
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

    // Both constraint systems share one Validate() per type: `@`-annotation
    // ValidationConstraints (in metadata) and `.`-control-operators (carried
    // inline on the field's type). The body is built first so the import block can
    // pull in a package only when a check that needs it actually lands.
    let mut body = String::new();
    let mut imports = ValidationImports::default();

    for rule in &input.csil_spec.rules {
        // A record rule reaches us as either `GroupDef` or a `TypeDef` wrapping a
        // `Group`; both produce a struct, so both must produce a Validate().
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            if !group.entries.iter().any(entry_has_check) {
                continue;
            }

            body.push_str(&format!(
                "// Validate{0} validates the {0} struct\n",
                rule.name
            ));
            body.push_str(&format!("func (v *{}) Validate() error {{\n", rule.name));

            for entry in &group.entries {
                if let Some(key) = &entry.key {
                    let field_name = go_field_name_from_key_with_metadata(key, &entry.metadata);
                    // An optional field is a Go pointer; every check on it is guarded
                    // and dereferenced so a nil optional is skipped rather than panicking.
                    let field = FieldRef {
                        name: &field_name,
                        optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
                    };

                    for metadata in &entry.metadata {
                        if let CsilFieldMetadata::Constraint(constraint) = metadata {
                            emit_metadata_constraint(
                                &mut body,
                                config,
                                field,
                                &entry.value_type,
                                constraint,
                                &mut imports,
                            );
                        }
                    }

                    if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                        for op in constraints {
                            emit_control_op_check(
                                &mut body,
                                config,
                                field,
                                &entry.value_type,
                                op,
                                &mut imports,
                            );
                        }
                    }
                }
            }

            body.push_str(&format!("{}return nil\n", config.indent_style));
            body.push_str("}\n\n");
        }
    }

    if body.is_empty() {
        return Ok(None);
    }

    // A `timestamp` bound is parsed at runtime via a package-local must-parser; emit
    // it once, only when a timestamp comparison actually landed.
    if imports.time {
        body.push_str(TIMESTAMP_HELPER_GO);
    }

    let mut content = String::new();
    content.push_str(&format!(
        "// Package {} contains generated validation functions.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    content.push_str("import (\n");
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    if imports.regexp {
        // regexp.MatchString backs both the `@regex` annotation and the `.regex`
        // control operator; it is imported only when a pattern check is emitted so
        // the file never carries an unused import (a Go compile error).
        content.push_str(&format!("{}\"regexp\"\n", config.indent_style));
    }
    if imports.time {
        // `time.Parse(time.RFC3339, ...)` parses a `timestamp` bound for comparison.
        content.push_str(&format!("{}\"time\"\n", config.indent_style));
    }
    if imports.decimal_lib {
        // Only the library decimal mapping references shopspring here; the default
        // CsilDecimal mapping compares via the in-package helper instead.
        content.push('\n');
        content.push_str(&format!(
            "{}\"github.com/shopspring/decimal\"\n",
            config.indent_style
        ));
    }
    content.push_str(")\n\n");

    content.push_str(&body);

    Ok(Some(content))
}

/// Which generated-import packages a Validate() body forces. Each is set only when
/// a check that needs it is emitted, so the import block never carries an unused
/// package (a Go compile error).
#[derive(Default)]
struct ValidationImports {
    regexp: bool,
    time: bool,
    decimal_lib: bool,
}

/// Runtime parser for an RFC3339 `timestamp` bound. The bound text is fixed at
/// generation time, so a parse failure is a generator bug, not bad runtime input —
/// hence the panic rather than a returned error.
const TIMESTAMP_HELPER_GO: &str = "\
// mustParseTimestamp parses an RFC3339 bound embedded at generation time. The text
// is fixed by the spec, so a parse failure signals a generator bug, not bad input.
func mustParseTimestamp(s string) time.Time {
\tt, err := time.Parse(time.RFC3339, s)
\tif err != nil {
\t\tpanic(err)
\t}
\treturn t
}
";

/// Whether a field's (possibly constrained) base type is an ordered core type that
/// needs a typed comparison rather than a plain `<`/`>` on a Go scalar: `decimal`
/// compares through its decimal library's `Cmp`, `timestamp` through `time.Time`'s
/// `Before`/`After`/`Equal`. Everything else is a numeric scalar.
enum OrderedKind {
    Numeric,
    Decimal,
    Timestamp,
}

fn ordered_field_kind(value_type: &CsilTypeExpression) -> OrderedKind {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    if let CsilTypeExpression::Builtin(name) = base {
        match name.as_str() {
            "decimal" => OrderedKind::Decimal,
            "timestamp" => OrderedKind::Timestamp,
            _ => OrderedKind::Numeric,
        }
    } else {
        OrderedKind::Numeric
    }
}

/// Escape a string for safe inclusion inside a Go double-quoted literal so an
/// embedded quote/backslash/newline can never break the surrounding literal.
fn go_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// A complete, always-valid Go double-quoted string literal for `s`.
fn go_string_lit(s: &str) -> String {
    format!("\"{}\"", go_escape(s))
}

/// A field's Go name plus whether it is optional (a Go pointer). Threaded through
/// the check emitters so each can consistently guard and dereference a nil optional.
#[derive(Clone, Copy)]
struct FieldRef<'a> {
    name: &'a str,
    optional: bool,
}

impl FieldRef<'_> {
    /// The expression that reads the field's value inside a check. An optional field
    /// is a pointer, so it is dereferenced explicitly; the surrounding check is
    /// guarded so the deref is never reached on a nil pointer.
    fn read_expr(&self) -> String {
        if self.optional {
            format!("(*v.{})", self.name)
        } else {
            format!("v.{}", self.name)
        }
    }
}

/// Emit a runtime check, guarding it behind a nil test when the field is optional.
/// An optional `decimal`/`timestamp`/`text` field is a pointer; reaching its value
/// (a `Cmp`, a `Before`/`After`, or a `len`/deref) on a nil pointer would panic, so
/// the check only runs when the pointer is set. A required field emits `check`
/// verbatim. `check` is authored at one indent level and re-indented under the guard.
fn push_optional_guard(content: &mut String, config: &GoConfig, field: FieldRef, check: &str) {
    if !field.optional {
        content.push_str(check);
        return;
    }
    let i = &config.indent_style;
    let name = field.name;
    content.push_str(&format!("{i}if v.{name} != nil {{\n"));
    for line in check.lines() {
        if line.is_empty() {
            content.push('\n');
        } else {
            content.push_str(i);
            content.push_str(line);
            content.push('\n');
        }
    }
    content.push_str(&format!("{i}}}\n"));
}

/// Emit a `len()`-based check (`@min-length`/`.size`/etc.) honoring optionality.
/// `message_tail` completes the phrasing after `field '<f>' must have `.
fn push_len_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    op: &str,
    n: u64,
    message_tail: &str,
) {
    let i = &config.indent_style;
    let access = field.read_expr();
    let name = field.name;
    let mut chk = String::new();
    chk.push_str(&format!("{i}if len({access}) {op} {n} {{\n"));
    chk.push_str(&format!(
        "{i}{i}return fmt.Errorf(\"field '{name}' must have {message_tail}\")\n"
    ));
    chk.push_str(&format!("{i}}}\n"));
    push_optional_guard(content, config, field, &chk);
}

/// The textual decimal bound for a `decimal` comparison. A `decimal` bound is
/// normally written as text (`.ge "0.00"`), but a bare numeric literal is accepted
/// and rendered as its canonical decimal text.
fn literal_as_decimal_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

/// The RFC3339 bound text for a `timestamp` comparison. Only a text literal is a
/// well-formed instant; anything else is skipped rather than emitting bad Go.
fn literal_as_timestamp_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// Emit one ordered comparison honoring the field's type. `go_op` is the Go
/// operator whose truth means the constraint is violated (e.g. `.ge` is violated
/// when the value is `<` the bound), and `desc` is the human phrasing the value
/// must satisfy. Numeric fields compare directly; `decimal`/`timestamp` fields
/// parse the bound and compare through the type's own ordering so the emitted Go
/// always compiles (never a scalar-vs-string compare).
fn emit_ordered_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
    imports: &mut ValidationImports,
) {
    let (go_op, desc) = op;
    let i = &config.indent_style;
    let access = field.read_expr();
    let name = field.name;
    match ordered_field_kind(value_type) {
        OrderedKind::Numeric => {
            let value_str = literal_value_to_go_string(value);
            let mut chk = String::new();
            chk.push_str(&format!("{i}if {access} {go_op} {value_str} {{\n"));
            chk.push_str(&format!(
                "{i}{i}return fmt.Errorf(\"field '{name}' must be {desc} {value_str}\")\n"
            ));
            chk.push_str(&format!("{i}}}\n"));
            push_optional_guard(content, config, field, &chk);
        }
        OrderedKind::Decimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let lit = go_string_lit(&text);
            // Both decimal libraries expose a sign-returning Cmp, so the same
            // `Cmp(bound) <go_op> 0` shape works for either mapping.
            let bound_expr = match config.decimal_mapping {
                DecimalMapping::Csil => format!("mustParseCsilDecimal({lit})"),
                DecimalMapping::Library => {
                    imports.decimal_lib = true;
                    format!("decimal.RequireFromString({lit})")
                }
            };
            let shown = go_escape(&text);
            let mut chk = String::new();
            chk.push_str(&format!("{i}if {access}.Cmp({bound_expr}) {go_op} 0 {{\n"));
            chk.push_str(&format!(
                "{i}{i}return fmt.Errorf(\"field '{name}' must be {desc} {shown}\")\n"
            ));
            chk.push_str(&format!("{i}}}\n"));
            push_optional_guard(content, config, field, &chk);
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_timestamp_text(value) else {
                return;
            };
            imports.time = true;
            let bound_expr = format!("mustParseTimestamp({})", go_string_lit(&text));
            // time.Time has no operators; translate the violation operator into the
            // matching Before/After/Equal expression.
            let cond = match go_op {
                "<" => format!("{access}.Before({bound_expr})"),
                ">" => format!("{access}.After({bound_expr})"),
                "<=" => format!("!{access}.After({bound_expr})"),
                ">=" => format!("!{access}.Before({bound_expr})"),
                "!=" => format!("!{access}.Equal({bound_expr})"),
                "==" => format!("{access}.Equal({bound_expr})"),
                _ => return,
            };
            let shown = go_escape(&text);
            let mut chk = String::new();
            chk.push_str(&format!("{i}if {cond} {{\n"));
            chk.push_str(&format!(
                "{i}{i}return fmt.Errorf(\"field '{name}' must be {desc} {shown}\")\n"
            ));
            chk.push_str(&format!("{i}}}\n"));
            push_optional_guard(content, config, field, &chk);
        }
    }
}

/// Whether an entry yields at least one runtime check. Encoding-only operators
/// (.bits/.and/.within/.json/.cbor/.cborseq) and `.default`/`@default` don't, so a
/// field carrying only those does not, by itself, warrant a Validate() function.
fn entry_has_check(entry: &CsilGroupEntry) -> bool {
    let meta_check = entry.metadata.iter().any(|m| match m {
        CsilFieldMetadata::Constraint(c) => constraint_is_check(c),
        _ => false,
    });
    let op_check = match &entry.value_type {
        CsilTypeExpression::Constrained { constraints, .. } => {
            constraints.iter().any(control_op_is_check)
        }
        _ => false,
    };
    meta_check || op_check
}

fn constraint_is_check(constraint: &CsilValidationConstraint) -> bool {
    // `@default` is a constructor concern, not a Validate() check; every other
    // annotation (including a `regex` Custom) produces one.
    match constraint {
        CsilValidationConstraint::Custom { name, .. } => name == "regex",
        _ => true,
    }
}

fn control_op_is_check(op: &CsilControlOperator) -> bool {
    matches!(
        op,
        CsilControlOperator::Size(_)
            | CsilControlOperator::Regex(_)
            | CsilControlOperator::GreaterEqual(_)
            | CsilControlOperator::LessEqual(_)
            | CsilControlOperator::GreaterThan(_)
            | CsilControlOperator::LessThan(_)
            | CsilControlOperator::Equal(_)
            | CsilControlOperator::NotEqual(_)
    )
}

/// Emit a single `@`-annotation ValidationConstraint as Go inside a Validate().
fn emit_metadata_constraint(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
    imports: &mut ValidationImports,
) {
    match constraint {
        CsilValidationConstraint::MinLength(min_len) => {
            let unit = if *min_len == 1 {
                "character"
            } else {
                "characters"
            };
            let tail = format!("at least {min_len} {unit}");
            push_len_check(content, config, field, "<", *min_len, &tail);
        }
        CsilValidationConstraint::MaxLength(max_len) => {
            let unit = if *max_len == 1 {
                "character"
            } else {
                "characters"
            };
            let tail = format!("at most {max_len} {unit}");
            push_len_check(content, config, field, ">", *max_len, &tail);
        }
        CsilValidationConstraint::MinItems(min_items) => {
            let unit = if *min_items == 1 { "item" } else { "items" };
            let tail = format!("at least {min_items} {unit}");
            push_len_check(content, config, field, "<", *min_items, &tail);
        }
        CsilValidationConstraint::MaxItems(max_items) => {
            let unit = if *max_items == 1 { "item" } else { "items" };
            let tail = format!("at most {max_items} {unit}");
            push_len_check(content, config, field, ">", *max_items, &tail);
        }
        // `@min-value`/`@max-value` are the annotation form of `.ge`/`.le`; route
        // them through the shared ordered emitter so a bound on a `decimal` or
        // `timestamp` field is parsed and typed-compared rather than compared as a
        // bare scalar (which would not compile).
        CsilValidationConstraint::MinValue(min_val) => {
            emit_ordered_check(
                content,
                config,
                field,
                value_type,
                ("<", "at least"),
                min_val,
                imports,
            );
        }
        CsilValidationConstraint::MaxValue(max_val) => {
            emit_ordered_check(
                content,
                config,
                field,
                value_type,
                (">", "at most"),
                max_val,
                imports,
            );
        }
        CsilValidationConstraint::Custom { name, value } => {
            if name == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                imports.regexp = true;
                emit_regex_check(content, config, field, pattern);
            }
        }
    }
}

/// Emit a single `.`-control-operator. Comparison and size/regex operators become
/// runtime checks; `.default` is applied by the constructor instead; the
/// encoding-only operators (.bits/.and/.within/.json/.cbor/.cborseq) leave a doc
/// comment so their presence is visible but they never fail validation.
fn emit_control_op_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
    imports: &mut ValidationImports,
) {
    let i = &config.indent_style;
    let field_name = field.name;
    // One match, one operator list: each comparison passes its `(violation-op,
    // phrasing)` pair to the shared emitter, which turns it into a numeric, decimal,
    // or timestamp comparison by field type. This avoids the prior split between a
    // dispatch table and dead `unreachable!` arms.
    let ordered = |content: &mut String, op_pair, v, imports: &mut ValidationImports| {
        emit_ordered_check(content, config, field, value_type, op_pair, v, imports);
    };
    match op {
        CsilControlOperator::GreaterEqual(v) => ordered(content, ("<", ">="), v, imports),
        CsilControlOperator::LessEqual(v) => ordered(content, (">", "<="), v, imports),
        CsilControlOperator::GreaterThan(v) => ordered(content, ("<=", ">"), v, imports),
        CsilControlOperator::LessThan(v) => ordered(content, (">=", "<"), v, imports),
        CsilControlOperator::Equal(v) => ordered(content, ("!=", "=="), v, imports),
        CsilControlOperator::NotEqual(v) => ordered(content, ("==", "!="), v, imports),
        CsilControlOperator::Size(size) => emit_size_check(content, config, field, size),
        CsilControlOperator::Regex(pattern) => {
            imports.regexp = true;
            emit_regex_check(content, config, field, pattern);
        }
        // Applied by the constructor (New<Type>), not validated here.
        CsilControlOperator::Default(_) => {}
        CsilControlOperator::Bits(bits) => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries .bits({bits}); a bit-set encoding hint, not a runtime check\n"
            ));
        }
        CsilControlOperator::And(_) => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries .and; intersection constraint left to the consumer\n"
            ));
        }
        CsilControlOperator::Within(_) => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries .within; range membership left to the consumer\n"
            ));
        }
        CsilControlOperator::Json | CsilControlOperator::Cbor | CsilControlOperator::Cborseq => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries an embedded-encoding operator; handled at (de)serialization, not validated\n"
            ));
        }
    }
}

/// `len()`-based check shared by `.size` forms; works for strings, byte slices,
/// arrays, and maps alike.
fn emit_size_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    size: &CsilSizeConstraint,
) {
    let mut one = |op: &str, n: u64, word: &str| {
        let tail = format!("{word} {n} elements");
        push_len_check(content, config, field, op, n, &tail);
    };
    match size {
        CsilSizeConstraint::Exact(n) => one("!=", *n, "exactly"),
        CsilSizeConstraint::Min(n) => one("<", *n, "at least"),
        CsilSizeConstraint::Max(n) => one(">", *n, "at most"),
        CsilSizeConstraint::Range { min, max } => {
            one("<", *min, "at least");
            one(">", *max, "at most");
        }
    }
}

fn emit_regex_check(content: &mut String, config: &GoConfig, field: FieldRef, pattern: &str) {
    let i = &config.indent_style;
    let access = field.read_expr();
    let name = field.name;
    // The pattern is rendered as a backtick raw literal for MatchString, but the
    // error message is a double-quoted literal: a raw pattern like `\d+` would form
    // an invalid Go escape there, so it is escaped to stay a well-formed literal.
    let shown = go_escape(pattern);
    let mut chk = String::new();
    chk.push_str(&format!(
        "{i}matched, _ := regexp.MatchString(`{pattern}`, {access})\n"
    ));
    chk.push_str(&format!("{i}if !matched {{\n"));
    chk.push_str(&format!(
        "{i}{i}return fmt.Errorf(\"field '{name}' must match pattern '{shown}'\")\n"
    ));
    chk.push_str(&format!("{i}}}\n"));
    push_optional_guard(content, config, field, &chk);
}

fn generate_constructors(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
    timestamp_helper_defined: bool,
) -> Result<Option<String>, i32> {
    // The constructor bodies are built first so the import block (and a possible
    // local timestamp must-parser) can be derived from what the typed defaults
    // actually reference, never carrying an unused import.
    let mut body = String::new();

    for rule in &input.csil_spec.rules {
        // A record rule reaches us as either `GroupDef` or a `TypeDef` wrapping a
        // `Group`; both produce a struct, so a default on either must be applied.
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        let Some(group) = group else { continue };

        // A default arrives either as the `@default(...)` annotation or the
        // `.default(...)` control operator on the field's type; both feed the same
        // constructor assignment.
        let fields_with_defaults: Vec<_> = group
            .entries
            .iter()
            .filter_map(|entry| {
                let key = entry.key.as_ref()?;
                let value = entry_default_value(entry)?;
                Some((
                    key,
                    value,
                    &entry.value_type,
                    &entry.occurrence,
                    &entry.metadata,
                ))
            })
            .collect();

        if fields_with_defaults.is_empty() {
            continue;
        }

        // Generate godoc comment with default values listed
        body.push_str(&format!(
            "// New{} creates a {} with default values:\n",
            rule.name, rule.name
        ));
        for (key, value, _, _, _) in &fields_with_defaults {
            let field_name = go_json_name_from_key(key);
            let value_str = literal_value_to_go_string(value);
            body.push_str(&format!("//   - {field_name}: {value_str}\n"));
        }
        body.push_str(&format!("func New{}() *{} {{\n", rule.name, rule.name));
        body.push_str(&format!(
            "{}return &{} {{\n",
            config.indent_style, rule.name
        ));

        for (key, value, value_type, occurrence, metadata) in &fields_with_defaults {
            let field_name = go_field_name_from_key_with_metadata(key, metadata);
            let go_value = literal_value_to_go_value(value, value_type, occurrence, config);
            body.push_str(&format!(
                "{}{}{}: {},\n",
                config.indent_style, config.indent_style, field_name, go_value
            ));
        }

        body.push_str(&format!("{}}}\n", config.indent_style));
        body.push_str("}\n\n");
    }

    if body.is_empty() {
        return Ok(None);
    }

    // The timestamp must-parser lives in the validation file when one is emitted; a
    // constructor with a timestamp default but no Validate() must carry its own copy
    // so the package still defines the symbol exactly once.
    let needs_ts_helper = body.contains("mustParseTimestamp(") && !timestamp_helper_defined;
    if needs_ts_helper {
        body.push_str(TIMESTAMP_HELPER_GO);
    }

    // A library-mapped decimal default constructs through `decimal.RequireFromString`
    // (and an optional one names `*decimal.Decimal`); only then is shopspring imported
    // here. The default CsilDecimal mapping resolves in-package, needing no import.
    let needs_shopspring = body.contains("decimal.");
    // The `time` package is named only when this file declares the timestamp helper
    // or constructs an optional `*time.Time` default; a bare must-parse call does not.
    let needs_time = needs_ts_helper || body.contains("time.Time");

    let mut content = String::new();
    content.push_str(&format!(
        "// Package {} contains generated constructor functions.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    if needs_time || needs_shopspring {
        content.push_str("import (\n");
        if needs_time {
            content.push_str(&format!("{}\"time\"\n", config.indent_style));
        }
        if needs_shopspring {
            if needs_time {
                content.push('\n');
            }
            content.push_str(&format!(
                "{}\"github.com/shopspring/decimal\"\n",
                config.indent_style
            ));
        }
        content.push_str(")\n\n");
    }

    content.push_str(&body);

    Ok(Some(content))
}

/// The default literal for a field, honoring both constraint systems: the
/// `@default(...)` annotation (carried in metadata) and the `.default(...)`
/// control operator (carried inline on the field's type). The annotation wins if
/// both are somehow present.
fn entry_default_value(entry: &CsilGroupEntry) -> Option<&CsilLiteralValue> {
    for metadata in &entry.metadata {
        if let CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { name, value }) =
            metadata
            && name == "default"
        {
            return Some(value);
        }
    }
    if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
        for op in constraints {
            if let CsilControlOperator::Default(value) = op {
                return Some(value);
            }
        }
    }
    None
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — that error half is the
/// Go `error` return, not part of the typed response. Without this the whole
/// union maps to the untyped `interface{}` fallback.
fn go_success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
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

/// Whether any type anywhere in the spec is the named builtin (e.g. `timestamp`
/// or `decimal`). Used to decide whether to import `time`, pull in shopspring, or
/// inject the `CsilDecimal` helper — none of which should appear when unused.
fn spec_uses_builtin(input: &WasmGeneratorInput, builtin: &str) -> bool {
    input
        .csil_spec
        .rules
        .iter()
        .any(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(group) => group
                .entries
                .iter()
                .any(|e| type_uses_builtin(&e.value_type, builtin)),
            CsilRuleType::TypeDef(type_expr) => type_uses_builtin(type_expr, builtin),
            CsilRuleType::ServiceDef(service) => service.operations.iter().any(|op| {
                type_uses_builtin(&op.input_type, builtin)
                    || type_uses_builtin(&op.output_type, builtin)
            }),
            _ => false,
        })
}

/// A push op (`-> Event`) carries a `null` input type. On a unary RPC there is
/// no request to send, so the request parameter is dropped rather than surfaced
/// as a meaningless `interface{}` the caller would have to pass `nil` for.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

fn type_uses_builtin(type_expr: &CsilTypeExpression, builtin: &str) -> bool {
    match type_expr {
        CsilTypeExpression::Builtin(name) => name == builtin,
        CsilTypeExpression::Array { element_type, .. } => type_uses_builtin(element_type, builtin),
        CsilTypeExpression::Map { key, value, .. } => {
            type_uses_builtin(key, builtin) || type_uses_builtin(value, builtin)
        }
        CsilTypeExpression::Choice(choices) => {
            choices.iter().any(|c| type_uses_builtin(c, builtin))
        }
        CsilTypeExpression::Constrained { base_type, .. } => type_uses_builtin(base_type, builtin),
        CsilTypeExpression::Group(group) | CsilTypeExpression::Tuple(group) => group
            .entries
            .iter()
            .any(|e| type_uses_builtin(&e.value_type, builtin)),
        _ => false,
    }
}

fn map_csil_type_to_go(
    type_expr: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
    decimal_type: &str,
) -> String {
    let base_type = match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" => "int64",
            "uint" => "uint64",
            "float" => "float64",
            // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`; the lexer
            // accepts both, so every generator maps the pair identically.
            "text" | "tstr" => "string",
            "bytes" | "bstr" => "[]byte",
            "bool" => "bool",
            // CBOR tag 0, RFC3339, always UTC per the wire contract; time.Time is
            // kept in UTC so encode/decode round-trips the `Z` offset.
            "timestamp" => "time.Time",
            // CBOR tag 4 exact decimal; the concrete Go type depends on the
            // decimal_mapping option (generated CsilDecimal vs. shopspring).
            "decimal" => decimal_type,
            "nil" | "null" => "interface{}",
            _ => name,
        },
        CsilTypeExpression::Reference(name) => name,
        CsilTypeExpression::Array { element_type, .. } => {
            let element = map_csil_type_to_go(element_type, &None, decimal_type);
            return format!("[]{element}");
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let key_type = map_csil_type_to_go(key, &None, decimal_type);
            let value_type = map_csil_type_to_go(value, &None, decimal_type);
            return format!("map[{key_type}]{value_type}");
        }
        // Go has no tuple type, so a fixed-shape array becomes an anonymous
        // struct rather than `[]interface{}`, preserving the per-position types.
        CsilTypeExpression::Tuple(group) => {
            return go_tuple_struct(&group.entries, decimal_type);
        }
        CsilTypeExpression::Constrained { base_type, .. } => {
            // Unwrap constrained types and map the base type
            // Constraints like .size, .default, .regex are validation rules, not Go types
            return map_csil_type_to_go(base_type, occurrence, decimal_type);
        }
        _ => "interface{}", // Fallback for complex types
    };

    // Handle occurrence
    match occurrence {
        Some(CsilOccurrence::Optional) => format!("*{base_type}"),
        _ => base_type.to_string(),
    }
}

/// Builds the anonymous Go struct that stands in for a CSIL tuple. Keeping it a
/// pure `entries -> String` mapping (instead of hoisting a named type) lets a
/// tuple slot in anywhere a type string is expected — top-level alias, struct
/// field, slice element, or map value — and stay type-safe. Keyed entries take
/// their key's name; positional ones fall back to `Field0`/`Field1`/….
fn go_tuple_struct(entries: &[CsilGroupEntry], decimal_type: &str) -> String {
    let fields: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let field_name = match &entry.key {
                Some(key) => go_field_name_from_key(key),
                None => format!("Field{index}"),
            };
            let field_type =
                map_csil_type_to_go(&entry.value_type, &entry.occurrence, decimal_type);
            format!("{field_name} {field_type}")
        })
        .collect();
    format!("struct {{ {} }}", fields.join("; "))
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

/// The `@depends-on(...)` boolean condition, surfaced as a Go-comment string on
/// the field. Go has no native conditional-presence facility, so — like the
/// simple `DependsOn` form — the dependency is documentation rather than enforced
/// code; rendering it keeps the intent visible to a reader of the generated type.
fn get_depends_comment(metadata: &[CsilFieldMetadata]) -> Option<String> {
    metadata
        .iter()
        .find_map(|meta| match meta {
            CsilFieldMetadata::DependsOnExpr(condition) => {
                Some(render_depends_condition(condition))
            }
            // The parser keeps the common `@depends-on(x = "y")` (and the bare
            // presence test `@depends-on(x)`) as the simple form, not the
            // expression form; rendering it here is what actually surfaces the
            // dependency the doc comment above promises to handle.
            CsilFieldMetadata::DependsOn { field, value } => Some(match value {
                Some(value) => {
                    format!("{field} == {}", literal_value_to_go_string(value))
                }
                None => field.clone(),
            }),
            _ => None,
        })
        // A rendered text value can carry a newline; since this lands in a `//`
        // line comment, an embedded break would push the remainder onto a second,
        // uncommented line and break the file. Keep the condition on one line.
        .map(|rendered| rendered.replace(['\n', '\r'], " "))
}

fn render_depends_condition(condition: &CsilDependsCondition) -> String {
    match condition {
        CsilDependsCondition::Compare { field, op, value } => match (op, value) {
            (Some(op), Some(value)) => format!(
                "{field} {} {}",
                depends_compare_op_str(op),
                literal_value_to_go_string(value)
            ),
            // A bare field (no operator/value) is a presence test.
            _ => field.clone(),
        },
        // `&` and `|` in the source map onto Go's `&&`/`||` so the rendered
        // comment reads like the boolean expression a Go author would write.
        CsilDependsCondition::All(conditions) => join_depends_conditions(conditions, "&&"),
        CsilDependsCondition::Any(conditions) => join_depends_conditions(conditions, "||"),
    }
}

fn join_depends_conditions(conditions: &[CsilDependsCondition], separator: &str) -> String {
    conditions
        .iter()
        .map(render_depends_condition)
        .collect::<Vec<_>>()
        .join(&format!(" {separator} "))
}

fn depends_compare_op_str(op: &CsilDependsCompareOp) -> &'static str {
    match op {
        CsilDependsCompareOp::Eq => "==",
        CsilDependsCompareOp::Ne => "!=",
        CsilDependsCompareOp::Lt => "<",
        CsilDependsCompareOp::Le => "<=",
        CsilDependsCompareOp::Gt => ">",
        CsilDependsCompareOp::Ge => ">=",
    }
}

fn literal_value_to_go_string(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{s}\""),
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
    value_type: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
    config: &GoConfig,
) -> String {
    let optional = matches!(occurrence, Some(CsilOccurrence::Optional));

    // A `decimal`/`timestamp` field is a typed Go value (CsilDecimal/shopspring's
    // Decimal/time.Time), never a Go string. A bare string literal assigned to such
    // a field would not compile, so the bound text is parsed into the typed value
    // via the same must-parsers the validation code uses.
    match ordered_field_kind(value_type) {
        OrderedKind::Decimal => {
            if let Some(text) = literal_as_decimal_text(value) {
                let lit = go_string_lit(&text);
                let expr = match config.decimal_mapping {
                    DecimalMapping::Csil => format!("mustParseCsilDecimal({lit})"),
                    DecimalMapping::Library => format!("decimal.RequireFromString({lit})"),
                };
                let go_type = config.decimal_go_type();
                return if optional {
                    format!("func() *{go_type} {{ v := {expr}; return &v }}()")
                } else {
                    expr
                };
            }
        }
        OrderedKind::Timestamp => {
            if let Some(text) = literal_as_timestamp_text(value) {
                let expr = format!("mustParseTimestamp({})", go_string_lit(&text));
                return if optional {
                    format!("func() *time.Time {{ v := {expr}; return &v }}()")
                } else {
                    expr
                };
            }
        }
        OrderedKind::Numeric => {}
    }

    let base_value = match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{s}\""),
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
                format!("func() *int64 {{ v := int64({i}); return &v }}()")
            }
            CsilLiteralValue::Float(f) => {
                format!("func() *float64 {{ v := float64({f}); return &v }}()")
            }
            CsilLiteralValue::Text(s) => {
                format!("func() *string {{ v := \"{s}\"; return &v }}()")
            }
            CsilLiteralValue::Bool(b) => format!("func() *bool {{ v := {b}; return &v }}()"),
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
            message: format!("Generator failed with error code: {error_code}"),
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
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("text".to_string()),
                &None,
                "CsilDecimal"
            ),
            "string"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("int".to_string()),
                &None,
                "CsilDecimal"
            ),
            "int64"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Reference("User".to_string()),
                &None,
                "CsilDecimal"
            ),
            "User"
        );
        // The CDDL aliases `tstr`/`bstr` map identically to `text`/`bytes`,
        // matching the rust and python generators.
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("tstr".to_string()),
                &None,
                "CsilDecimal"
            ),
            "string"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("bstr".to_string()),
                &None,
                "CsilDecimal"
            ),
            "[]byte"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("bytes".to_string()),
                &None,
                "CsilDecimal"
            ),
            "[]byte"
        );
    }

    #[test]
    fn test_timestamp_and_decimal_type_mapping() {
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("timestamp".to_string()),
                &None,
                "CsilDecimal"
            ),
            "time.Time"
        );
        // The decimal Go type is whatever the active mapping passes through.
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("decimal".to_string()),
                &None,
                "CsilDecimal"
            ),
            "CsilDecimal"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("decimal".to_string()),
                &None,
                "decimal.Decimal"
            ),
            "decimal.Decimal"
        );
    }

    #[test]
    fn test_optional_types() {
        let optional = Some(CsilOccurrence::Optional);
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("text".to_string()),
                &optional,
                "CsilDecimal"
            ),
            "*string"
        );
    }

    fn input_with_service(name: &str, ops: Vec<CsilServiceOperation>) -> WasmGeneratorInput {
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: name.to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: ops,
                        wire_id: None,
                    }),
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
            wire_id: None,
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
        let config = GoConfig::from_options(&input.config.options).unwrap();
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
        let config = GoConfig::from_options(&input.config.options).unwrap();
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
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        assert!(!services.contains("type Codec interface"));
        assert!(!services.contains("RouteAuthChannel"));
        assert!(!services.contains("EncodeAuthLogin"));
        // "fmt" should not be imported when no router exists.
        assert!(!services.contains("\"fmt\""));
    }

    fn unary_union_op(name: &str) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
            output_type: CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                CsilTypeExpression::Reference("ServiceError".to_string()),
            ]),
            direction: CsilServiceDirection::Unidirectional,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        }
    }

    #[test]
    fn cbor_tags_key_by_csil_field_name() {
        use csilgen_common::{CsilGroupEntry, CsilGroupExpression};
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: "Task".to_string(),
                    rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![
                            CsilGroupEntry {
                                key: Some(CsilGroupKey::Bare("current_state".to_string())),
                                value_type: CsilTypeExpression::Builtin("text".to_string()),
                                occurrence: None,
                                metadata: vec![],
                                doc_comments: Vec::new(),
                            },
                            CsilGroupEntry {
                                key: Some(CsilGroupKey::Bare("note".to_string())),
                                value_type: CsilTypeExpression::Builtin("text".to_string()),
                                occurrence: Some(CsilOccurrence::Optional),
                                metadata: vec![],
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
        };
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        // Wire key is the CSIL field name verbatim, alongside the existing tags.
        assert!(
            types
                .contains("`json:\"current_state\" yaml:\"current_state\" cbor:\"current_state\"`")
        );
        assert!(types.contains("cbor:\"note,omitempty\""));
    }

    #[test]
    fn typed_response_strips_service_error() {
        let input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(services.contains(
            "SubmitTask(ctx context.Context, req SubmitTaskRequest) (SubmitTaskResponse, error)"
        ));
        assert!(!services.contains("interface{}"));
    }

    #[test]
    fn go_client_target_emits_typed_client() {
        let mut input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        input.config.target = "go-client".to_string();
        let output = super::process_generation(input).expect("generation ok");
        let client = output
            .files
            .iter()
            .find(|f| f.path == "client.gen.go")
            .expect("client.gen.go emitted");
        assert!(client.content.contains("type Transport interface"));
        assert!(client.content.contains("type ClientError struct"));
        assert!(
            client
                .content
                .contains("func NewCorndogsClient(transport Transport) *CorndogsClient")
        );
        assert!(client.content.contains(
            "func (c *CorndogsClient) SubmitTask(ctx context.Context, req SubmitTaskRequest) (SubmitTaskResponse, error)"
        ));
        assert!(
            client
                .content
                .contains("err := c.transport.Call(ctx, \"corndogs\", \"SubmitTask\", req, &resp)")
        );
        // Server interface must not be emitted for the client target.
        assert!(!output.files.iter().any(|f| f.path == "services.gen.go"));
    }

    #[test]
    fn go_server_alias_and_typesonly() {
        let mut input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        input.config.target = "go-server".to_string();
        let output = super::process_generation(input).expect("generation ok");
        assert!(output.files.iter().any(|f| f.path == "services.gen.go"));
        assert!(!output.files.iter().any(|f| f.path == "client.gen.go"));

        let mut input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        input.config.target = "go-typesonly".to_string();
        let output = super::process_generation(input).expect("generation ok");
        // This spec has no type rules, so the service surface is simply absent.
        assert!(!output.files.iter().any(|f| f.path == "services.gen.go"));
        assert!(!output.files.iter().any(|f| f.path == "client.gen.go"));
    }

    #[test]
    fn unknown_go_subtarget_errors() {
        let mut input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        input.config.target = "go-bogus".to_string();
        assert!(super::process_generation(input).is_err());
    }

    fn group_input(
        type_name: &str,
        entries: Vec<CsilGroupEntry>,
        options: HashMap<String, serde_json::Value>,
    ) -> WasmGeneratorInput {
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: type_name.to_string(),
                    rule_type: CsilRuleType::GroupDef(csilgen_common::CsilGroupExpression {
                        entries,
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
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "go".to_string(),
                output_dir: "/tmp".to_string(),
                options,
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

    fn string_opts(pairs: &[(&str, &str)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    fn bare_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type,
            occurrence: None,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    fn constrained_entry(
        name: &str,
        base: &str,
        constraints: Vec<CsilControlOperator>,
    ) -> CsilGroupEntry {
        bare_entry(
            name,
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin(base.to_string())),
                constraints,
            },
        )
    }

    #[test]
    fn timestamp_maps_to_time_and_imports_time() {
        let input = group_input(
            "Event",
            vec![bare_entry(
                "created_at",
                CsilTypeExpression::Builtin("timestamp".to_string()),
            )],
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("CreatedAt time.Time"));
        assert!(types.contains("import (\n\t\"time\"\n)"));
    }

    #[test]
    fn decimal_csil_mode_emits_self_contained_helper() {
        let input = group_input(
            "Money",
            vec![bare_entry(
                "amount",
                CsilTypeExpression::Builtin("decimal".to_string()),
            )],
            HashMap::new(),
        );
        let output = super::process_generation(input).expect("generation ok");

        let types = output
            .files
            .iter()
            .find(|f| f.path == "types.gen.go")
            .expect("types emitted");
        assert!(types.content.contains("Amount CsilDecimal"));
        // Default mode must not pull in shopspring anywhere.
        assert!(!types.content.contains("shopspring"));

        let helper = output
            .files
            .iter()
            .find(|f| f.path == "csil_decimal.gen.go")
            .expect("CsilDecimal helper emitted");
        assert!(helper.content.contains("type CsilDecimal struct"));
        assert!(
            helper
                .content
                .contains("func (d CsilDecimal) MarshalCBOR()")
        );
        // CBOR tag 4 decimal fraction is the normative wire form.
        assert!(helper.content.contains("Number:  4,"));
        assert!(
            helper
                .content
                .contains("[]interface{}{d.Exponent, d.mantissa()}")
        );
        assert!(helper.content.contains("\"github.com/fxamacker/cbor/v2\""));
        // Interop bridge present, but no hard dependency on shopspring.
        assert!(helper.content.contains("func ParseCsilDecimal"));
        assert!(
            helper
                .content
                .contains("func (d CsilDecimal) String() string")
        );
        // The bridge is documented, but shopspring is never imported.
        assert!(!helper.content.contains("\"github.com/shopspring/decimal\""));
    }

    #[test]
    fn decimal_library_mode_uses_shopspring_and_no_helper() {
        let input = group_input(
            "Money",
            vec![bare_entry(
                "amount",
                CsilTypeExpression::Builtin("decimal".to_string()),
            )],
            string_opts(&[("decimal_mapping", "library")]),
        );
        let output = super::process_generation(input).expect("generation ok");

        let types = output
            .files
            .iter()
            .find(|f| f.path == "types.gen.go")
            .expect("types emitted");
        assert!(types.content.contains("Amount decimal.Decimal"));
        assert!(types.content.contains("\"github.com/shopspring/decimal\""));
        // The library type stands alone; no generated helper.
        assert!(!output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
    }

    #[test]
    fn csil_decimal_helper_absent_when_decimal_unused() {
        let input = group_input(
            "Plain",
            vec![bare_entry(
                "name",
                CsilTypeExpression::Builtin("text".to_string()),
            )],
            HashMap::new(),
        );
        let output = super::process_generation(input).expect("generation ok");
        assert!(!output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
        let types = output
            .files
            .iter()
            .find(|f| f.path == "types.gen.go")
            .expect("types emitted");
        assert!(!types.content.contains("\"time\""));
    }

    #[test]
    fn unknown_decimal_mapping_is_hard_error() {
        let input = group_input(
            "Money",
            vec![bare_entry(
                "amount",
                CsilTypeExpression::Builtin("decimal".to_string()),
            )],
            string_opts(&[("decimal_mapping", "bogus")]),
        );
        assert!(super::process_generation(input).is_err());
    }

    #[test]
    fn control_operators_emit_validation_checks() {
        let entries = vec![
            constrained_entry(
                "username",
                "text",
                vec![
                    CsilControlOperator::Size(CsilSizeConstraint::Range { min: 3, max: 20 }),
                    CsilControlOperator::Regex("^[a-z]+$".to_string()),
                ],
            ),
            constrained_entry(
                "age",
                "int",
                vec![
                    CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(18)),
                    CsilControlOperator::LessEqual(CsilLiteralValue::Integer(120)),
                ],
            ),
            // Encoding-only operator: documented, never a check, never an error.
            constrained_entry("blob", "bytes", vec![CsilControlOperator::Cbor]),
        ];
        let input = group_input("Account", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        assert!(validation.contains("func (v *Account) Validate() error"));
        assert!(validation.contains("if len(v.Username) < 3 {"));
        assert!(validation.contains("if len(v.Username) > 20 {"));
        assert!(validation.contains("regexp.MatchString(`^[a-z]+$`, v.Username)"));
        // regexp is imported only because a pattern check landed.
        assert!(validation.contains("\"regexp\""));
        assert!(validation.contains("if v.Age < 18 {"));
        assert!(validation.contains("if v.Age > 120 {"));
        assert!(validation.contains("// field 'Blob' carries an embedded-encoding operator"));
    }

    #[test]
    fn both_constraint_systems_coexist_in_validate() {
        let entries = vec![
            CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("name".to_string())),
                value_type: CsilTypeExpression::Builtin("text".to_string()),
                occurrence: None,
                metadata: vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::MinLength(2),
                )],
                doc_comments: Vec::new(),
            },
            constrained_entry(
                "count",
                "int",
                vec![CsilControlOperator::GreaterThan(CsilLiteralValue::Integer(
                    0,
                ))],
            ),
        ];
        let input = group_input("Mix", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        // The `@`-annotation and `.`-control-operator both land in one Validate().
        assert!(validation.contains("if len(v.Name) < 2 {"));
        assert!(validation.contains("if v.Count <= 0 {"));
        // No regex here, so regexp must not be imported.
        assert!(!validation.contains("\"regexp\""));
    }

    #[test]
    fn validation_skipped_when_only_encoding_operators() {
        let input = group_input(
            "Blobby",
            vec![constrained_entry(
                "raw",
                "bytes",
                vec![CsilControlOperator::Cborseq],
            )],
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        // An encoding-only operator yields no runtime check, so no Validate() file.
        assert!(
            super::generate_validation(&input, &config, &mut Vec::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn default_control_operator_feeds_constructor() {
        let input = group_input(
            "Config",
            vec![constrained_entry(
                "retries",
                "int",
                vec![CsilControlOperator::Default(CsilLiteralValue::Integer(3))],
            )],
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let ctors = super::generate_constructors(&input, &config, &mut Vec::new(), false)
            .unwrap()
            .expect("constructors emitted");
        assert!(ctors.contains("func NewConfig() *Config"));
        assert!(ctors.contains("Retries: 3,"));
    }

    fn decimal_and_timestamp_bound_entries() -> Vec<CsilGroupEntry> {
        vec![
            constrained_entry(
                "balance",
                "decimal",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "0.00".to_string(),
                ))],
            ),
            constrained_entry(
                "created_at",
                "timestamp",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                ))],
            ),
        ]
    }

    #[test]
    fn decimal_and_timestamp_bounds_parse_not_bare_compare_csil_mode() {
        let input = group_input(
            "User",
            decimal_and_timestamp_bound_entries(),
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        assert!(validation.contains("func (v *User) Validate() error"));

        // The decimal bound is parsed and compared through Cmp, never `v.Balance < "0.00"`.
        assert!(validation.contains("if v.Balance.Cmp(mustParseCsilDecimal(\"0.00\")) < 0 {"));
        assert!(!validation.contains("v.Balance < \"0.00\""));

        // The timestamp bound is parsed via RFC3339 and compared with Before.
        assert!(
            validation
                .contains("if v.CreatedAt.Before(mustParseTimestamp(\"1970-01-01T00:00:00Z\")) {")
        );
        assert!(!validation.contains("v.CreatedAt < \"1970-01-01T00:00:00Z\""));
        assert!(validation.contains("func mustParseTimestamp(s string) time.Time"));

        // time is imported because a timestamp comparison landed; the default
        // decimal mapping never references shopspring.
        assert!(validation.contains("\"time\""));
        assert!(!validation.contains("shopspring"));
        assert!(!validation.contains("decimal.RequireFromString"));
    }

    #[test]
    fn decimal_bound_uses_shopspring_in_library_mode() {
        let input = group_input(
            "User",
            decimal_and_timestamp_bound_entries(),
            string_opts(&[("decimal_mapping", "library")]),
        );
        let output = super::process_generation(input).expect("generation ok");
        let validation = output
            .files
            .iter()
            .find(|f| f.path == "validation.gen.go")
            .expect("validation emitted");

        // Library mode compares through shopspring's RequireFromString/Cmp and must
        // import the package in the validation file itself.
        assert!(
            validation
                .content
                .contains("if v.Balance.Cmp(decimal.RequireFromString(\"0.00\")) < 0 {")
        );
        assert!(
            validation
                .content
                .contains("\"github.com/shopspring/decimal\"")
        );
        assert!(!validation.content.contains("mustParseCsilDecimal"));
        // No CsilDecimal helper file in library mode.
        assert!(!output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
    }

    #[test]
    fn csil_decimal_helper_carries_cmp_and_must_parse() {
        // The Cmp method and must-parser the validation file relies on live in the
        // generated helper, so a decimal Validate() compiles against the same package.
        let input = group_input(
            "User",
            decimal_and_timestamp_bound_entries(),
            HashMap::new(),
        );
        let output = super::process_generation(input).expect("generation ok");
        let helper = output
            .files
            .iter()
            .find(|f| f.path == "csil_decimal.gen.go")
            .expect("CsilDecimal helper emitted");
        assert!(
            helper
                .content
                .contains("func (d CsilDecimal) Cmp(other CsilDecimal) int")
        );
        assert!(
            helper
                .content
                .contains("func mustParseCsilDecimal(s string) CsilDecimal")
        );
    }

    #[test]
    fn min_value_annotation_on_decimal_field_parses_bound() {
        // `@min-value` is the annotation form; it must get the same typed-compare
        // treatment as `.ge` so it does not emit a bare scalar comparison.
        let entry = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("balance".to_string())),
            value_type: CsilTypeExpression::Builtin("decimal".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinValue(CsilLiteralValue::Text("0.00".to_string())),
            )],
            doc_comments: Vec::new(),
        };
        let input = group_input("Wallet", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        assert!(validation.contains("if v.Balance.Cmp(mustParseCsilDecimal(\"0.00\")) < 0 {"));
        assert!(validation.contains("must be at least 0.00"));
        assert!(!validation.contains("v.Balance < \"0.00\""));
    }

    #[test]
    fn bound_with_embedded_quote_stays_a_valid_literal() {
        // A pathological bound must never break the surrounding Go string literal;
        // the embedded quote is escaped in both the parse argument and the message.
        let entry = constrained_entry(
            "label",
            "timestamp",
            vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                "a\"b".to_string(),
            ))],
        );
        let input = group_input("Weird", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        assert!(validation.contains("mustParseTimestamp(\"a\\\"b\")"));
        assert!(validation.contains("must be >= a\\\"b"));
    }

    fn optional_constrained_entry(
        name: &str,
        base: &str,
        constraints: Vec<CsilControlOperator>,
    ) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin(base.to_string())),
                constraints,
            },
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn regex_message_escapes_pattern_so_go_literal_stays_valid() {
        // A pattern with a backslash escape (`\d+`) must not be spliced raw into the
        // double-quoted error message: `\d` is an invalid Go escape and would not
        // compile. The MatchString call keeps the raw backtick form.
        let entry = constrained_entry(
            "code",
            "text",
            vec![CsilControlOperator::Regex(r"\d+".to_string())],
        );
        let input = group_input("Ticket", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        // MatchString still uses the raw backtick literal verbatim.
        assert!(validation.contains("regexp.MatchString(`\\d+`, v.Code)"));
        // The message escapes the backslash so the double-quoted literal is valid.
        assert!(validation.contains("must match pattern '\\\\d+'"));
        // The invalid single-backslash form must never appear in the message.
        assert!(!validation.contains("must match pattern '\\d+'"));
    }

    #[test]
    fn optional_fields_are_nil_guarded_and_dereferenced() {
        // Optional fields are Go pointers; dereferencing a nil one in Validate()
        // would panic. Every check must sit behind a nil guard and read through a
        // deref so a missing optional is simply skipped.
        let entries = vec![
            optional_constrained_entry(
                "balance",
                "decimal",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "0.00".to_string(),
                ))],
            ),
            optional_constrained_entry(
                "created_at",
                "timestamp",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                ))],
            ),
            CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("name".to_string())),
                value_type: CsilTypeExpression::Builtin("text".to_string()),
                occurrence: Some(CsilOccurrence::Optional),
                metadata: vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::MinLength(2),
                )],
                doc_comments: Vec::new(),
            },
        ];
        let input = group_input("Account", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        // Decimal: guarded, and the pointer is dereferenced for the Cmp.
        assert!(validation.contains("if v.Balance != nil {"));
        assert!(validation.contains("if (*v.Balance).Cmp(mustParseCsilDecimal(\"0.00\")) < 0 {"));
        // Timestamp: guarded, dereferenced for Before.
        assert!(validation.contains("if v.CreatedAt != nil {"));
        assert!(
            validation.contains(
                "if (*v.CreatedAt).Before(mustParseTimestamp(\"1970-01-01T00:00:00Z\")) {"
            )
        );
        // String length: guarded, dereferenced for len.
        assert!(validation.contains("if v.Name != nil {"));
        assert!(validation.contains("if len((*v.Name)) < 2 {"));
    }

    #[test]
    fn typed_defaults_construct_decimal_and_timestamp_values() {
        // A `decimal`/`timestamp` default must build the typed Go value, never a bare
        // string literal assigned to a CsilDecimal/time.Time field (a compile error).
        let entries = vec![
            constrained_entry(
                "balance",
                "decimal",
                vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                    "0.00".to_string(),
                ))],
            ),
            constrained_entry(
                "created_at",
                "timestamp",
                vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                ))],
            ),
        ];
        let input = group_input("Wallet", entries, HashMap::new());
        let output = super::process_generation(input).expect("generation ok");
        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");

        assert!(
            ctors
                .content
                .contains("Balance: mustParseCsilDecimal(\"0.00\"),")
        );
        assert!(
            ctors
                .content
                .contains("CreatedAt: mustParseTimestamp(\"1970-01-01T00:00:00Z\"),")
        );
        // No Validate() lands here (defaults are not checks), so the constructor file
        // carries its own copy of the timestamp must-parser and imports time.
        assert!(
            ctors
                .content
                .contains("func mustParseTimestamp(s string) time.Time")
        );
        assert!(ctors.content.contains("\"time\""));
        // The CsilDecimal must-parser is provided by the helper file, in-package.
        assert!(output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
    }

    #[test]
    fn timestamp_default_does_not_redeclare_helper_when_validation_defines_it() {
        // When a timestamp field has both a bound (Validate() defines the must-parser)
        // and a default (constructor references it), the helper is defined once.
        let entry = constrained_entry(
            "created_at",
            "timestamp",
            vec![
                CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                )),
                CsilControlOperator::Default(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                )),
            ],
        );
        let input = group_input("Event", vec![entry], HashMap::new());
        let output = super::process_generation(input).expect("generation ok");

        let validation = output
            .files
            .iter()
            .find(|f| f.path == "validation.gen.go")
            .expect("validation emitted");
        assert!(
            validation
                .content
                .contains("func mustParseTimestamp(s string) time.Time")
        );

        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");
        // The constructor references the must-parser but must not redeclare it.
        assert!(
            ctors
                .content
                .contains("CreatedAt: mustParseTimestamp(\"1970-01-01T00:00:00Z\"),")
        );
        assert!(!ctors.content.contains("func mustParseTimestamp"));
    }

    #[test]
    fn library_decimal_default_uses_shopspring_and_imports_it() {
        let entry = constrained_entry(
            "balance",
            "decimal",
            vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                "0.00".to_string(),
            ))],
        );
        let input = group_input(
            "Wallet",
            vec![entry],
            string_opts(&[("decimal_mapping", "library")]),
        );
        let output = super::process_generation(input).expect("generation ok");
        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");
        assert!(
            ctors
                .content
                .contains("Balance: decimal.RequireFromString(\"0.00\"),")
        );
        assert!(ctors.content.contains("\"github.com/shopspring/decimal\""));
    }

    #[test]
    fn typedef_group_record_gets_constructor_for_defaults() {
        // A record authored as a `TypeDef` wrapping a `Group` must apply defaults just
        // like a `GroupDef`; the constructor path handles both rule shapes.
        let group = csilgen_common::CsilGroupExpression {
            entries: vec![constrained_entry(
                "retries",
                "int",
                vec![CsilControlOperator::Default(CsilLiteralValue::Integer(3))],
            )],
        };
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: "Config".to_string(),
                    rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(group)),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 0,
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
        };
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let ctors = super::generate_constructors(&input, &config, &mut Vec::new(), false)
            .unwrap()
            .expect("constructors emitted");
        assert!(ctors.contains("func NewConfig() *Config"));
        assert!(ctors.contains("Retries: 3,"));
    }

    #[test]
    fn decimal_integer_bound_and_default_render_as_decimal_text() {
        // A `decimal` bound/default written as a bare integer literal is rendered to
        // its decimal string and parsed, not only handled when it arrives as text.
        let entry = constrained_entry(
            "balance",
            "decimal",
            vec![
                CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(5)),
                CsilControlOperator::Default(CsilLiteralValue::Integer(0)),
            ],
        );
        let input = group_input("Wallet", vec![entry], HashMap::new());
        let output = super::process_generation(input).expect("generation ok");

        let validation = output
            .files
            .iter()
            .find(|f| f.path == "validation.gen.go")
            .expect("validation emitted");
        assert!(
            validation
                .content
                .contains("if v.Balance.Cmp(mustParseCsilDecimal(\"5\")) < 0 {")
        );

        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");
        assert!(
            ctors
                .content
                .contains("Balance: mustParseCsilDecimal(\"0\"),")
        );
    }

    #[test]
    fn equality_operators_still_emit_after_match_collapse() {
        // Collapsing the comparison dispatch must not drop any operator: `.eq`/`.ne`
        // still produce their checks.
        let entries = vec![
            constrained_entry(
                "exact",
                "int",
                vec![CsilControlOperator::Equal(CsilLiteralValue::Integer(7))],
            ),
            constrained_entry(
                "forbidden",
                "int",
                vec![CsilControlOperator::NotEqual(CsilLiteralValue::Integer(13))],
            ),
        ];
        let input = group_input("Limits", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        assert!(validation.contains("if v.Exact != 7 {"));
        assert!(validation.contains("if v.Forbidden == 13 {"));
    }

    fn tuple_entry(
        key: Option<&str>,
        value_type: CsilTypeExpression,
        occurrence: Option<CsilOccurrence>,
    ) -> CsilGroupEntry {
        CsilGroupEntry {
            key: key.map(|k| CsilGroupKey::Bare(k.to_string())),
            value_type,
            occurrence,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn positional_tuple_maps_to_anonymous_struct() {
        // `[text, ?int, bool]` has no keys, so entries become Field0/Field1/Field2;
        // the optional entry keeps its pointer mapping inside the struct.
        let tuple = CsilTypeExpression::Tuple(csilgen_common::CsilGroupExpression {
            entries: vec![
                tuple_entry(None, CsilTypeExpression::Builtin("text".to_string()), None),
                tuple_entry(
                    None,
                    CsilTypeExpression::Builtin("int".to_string()),
                    Some(CsilOccurrence::Optional),
                ),
                tuple_entry(None, CsilTypeExpression::Builtin("bool".to_string()), None),
            ],
        });
        assert_eq!(
            map_csil_type_to_go(&tuple, &None, "CsilDecimal"),
            "struct { Field0 string; Field1 *int64; Field2 bool }"
        );
    }

    #[test]
    fn keyed_tuple_uses_keys_for_field_names() {
        // `[tag: text, value: any]` names fields after its keys.
        let tuple = CsilTypeExpression::Tuple(csilgen_common::CsilGroupExpression {
            entries: vec![
                tuple_entry(
                    Some("tag"),
                    CsilTypeExpression::Builtin("text".to_string()),
                    None,
                ),
                tuple_entry(
                    Some("value"),
                    CsilTypeExpression::Builtin("any".to_string()),
                    None,
                ),
            ],
        });
        assert_eq!(
            map_csil_type_to_go(&tuple, &None, "CsilDecimal"),
            "struct { Tag string; Value any }"
        );
    }

    #[test]
    fn tuple_typedef_emits_named_struct() {
        // A top-level tuple alias resolves to a named Go struct, so it stays
        // type-safe rather than collapsing to interface{}.
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: "MixedArray".to_string(),
                    rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Tuple(
                        csilgen_common::CsilGroupExpression {
                            entries: vec![
                                tuple_entry(
                                    None,
                                    CsilTypeExpression::Builtin("text".to_string()),
                                    None,
                                ),
                                tuple_entry(
                                    None,
                                    CsilTypeExpression::Builtin("int".to_string()),
                                    None,
                                ),
                            ],
                        },
                    )),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 0,
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
        };
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("type MixedArray struct { Field0 string; Field1 int64 }"));
    }

    #[test]
    fn tuple_carrying_timestamp_pulls_time_import() {
        // A tuple entry typed `timestamp` must count toward the `time` import the
        // same way an array or group entry would.
        let tuple = CsilTypeExpression::Tuple(csilgen_common::CsilGroupExpression {
            entries: vec![tuple_entry(
                Some("at"),
                CsilTypeExpression::Builtin("timestamp".to_string()),
                None,
            )],
        });
        assert!(type_uses_builtin(&tuple, "timestamp"));
        assert!(!type_uses_builtin(&tuple, "decimal"));
    }

    #[test]
    fn depends_on_expr_renders_boolean_tree() {
        // All -> &&, Any -> ||, with comparison and presence leaves.
        let condition = CsilDependsCondition::Any(vec![
            CsilDependsCondition::All(vec![
                CsilDependsCondition::Compare {
                    field: "account_type".to_string(),
                    op: Some(CsilDependsCompareOp::Eq),
                    value: Some(CsilLiteralValue::Text("enterprise".to_string())),
                },
                CsilDependsCondition::Compare {
                    field: "seats".to_string(),
                    op: Some(CsilDependsCompareOp::Gt),
                    value: Some(CsilLiteralValue::Integer(5)),
                },
            ]),
            // A bare field is a presence test, no operator.
            CsilDependsCondition::Compare {
                field: "override_flag".to_string(),
                op: None,
                value: None,
            },
        ]);
        assert_eq!(
            render_depends_condition(&condition),
            "account_type == \"enterprise\" && seats > 5 || override_flag"
        );
    }

    #[test]
    fn depends_on_expr_emits_field_comment() {
        // The dependency survives generation as a Go comment on the field.
        let entry = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("state".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![CsilFieldMetadata::DependsOnExpr(
                CsilDependsCondition::Compare {
                    field: "country".to_string(),
                    op: Some(CsilDependsCompareOp::Ne),
                    value: Some(CsilLiteralValue::Text("US".to_string())),
                },
            )],
            doc_comments: Vec::new(),
        };
        let input = group_input("ShippingForm", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("// depends-on: country != \"US\""));
    }

    #[test]
    fn reverse_op_with_null_input_emits_encoder_without_request_param() {
        // `op: <- Event` yields a null input on a Reverse op; it must produce a
        // server-push encoder keyed on the output type and never a request param.
        let push_op = CsilServiceOperation {
            name: "user-joined".to_string(),
            input_type: CsilTypeExpression::Builtin("null".to_string()),
            output_type: CsilTypeExpression::Reference("UserJoinedEvent".to_string()),
            direction: CsilServiceDirection::Reverse,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        };
        let input = input_with_service("ChatService", vec![push_op]);
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Push-only op rides the encoder surface, typed by the event it sends.
        assert!(
            services.contains("func EncodeChatServiceUserJoined(codec Codec, msg UserJoinedEvent)")
        );
        // No inbound interface method and no bogus request parameter for a push op.
        assert!(!services.contains("UserJoined(ctx context.Context, req"));
        assert!(!services.contains("UserJoined(ctx context.Context, msg"));
    }

    #[test]
    fn simple_depends_on_emits_field_comment() {
        // The parser keeps `@depends-on(x = "y")` as the simple `DependsOn` form;
        // both a string comparison and a boolean comparison must surface as a Go
        // comment rather than being silently dropped.
        let text_dep = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("region".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![CsilFieldMetadata::DependsOn {
                field: "country".to_string(),
                value: Some(CsilLiteralValue::Text("US".to_string())),
            }],
            doc_comments: Vec::new(),
        };
        let bool_dep = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("tax_id".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![CsilFieldMetadata::DependsOn {
                field: "is_business".to_string(),
                value: Some(CsilLiteralValue::Bool(true)),
            }],
            doc_comments: Vec::new(),
        };
        let input = group_input("Address", vec![text_dep, bool_dep], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("// depends-on: country == \"US\""));
        assert!(types.contains("// depends-on: is_business == true"));
    }

    #[test]
    fn unidirectional_op_with_null_input_omits_request_param() {
        // `op: -> Event` carries a null input on a unary op; neither the client
        // method nor the interface method should surface a meaningless request
        // parameter the caller would have to pass `nil` for.
        let push_op = CsilServiceOperation {
            name: "ping".to_string(),
            input_type: CsilTypeExpression::Builtin("null".to_string()),
            output_type: CsilTypeExpression::Reference("Pong".to_string()),
            direction: CsilServiceDirection::Unidirectional,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        };
        let input = input_with_service("HealthService", vec![push_op]);
        let config = GoConfig::from_options(&input.config.options).unwrap();

        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(services.contains("Ping(ctx context.Context) (Pong, error)"));
        assert!(!services.contains("Ping(ctx context.Context, req"));

        let client = super::generate_client(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("client emitted");
        assert!(client.contains("func (c *HealthClient) Ping(ctx context.Context) (Pong, error)"));
        assert!(!client.contains("Ping(ctx context.Context, req"));
        // The transport still needs a payload arg; a null input passes nil.
        assert!(client.contains("c.transport.Call(ctx, \"health\", \"Ping\", nil, &resp)"));
    }

    fn wire_id_input() -> WasmGeneratorInput {
        let mut place = make_op(
            "place-order",
            "Order",
            "Receipt",
            CsilServiceDirection::Unidirectional,
        );
        place.wire_id = Some(7);
        let cancel = make_op(
            "cancel-order",
            "Order",
            "Receipt",
            CsilServiceDirection::Unidirectional,
        );
        let mut input = input_with_service("OrderService", vec![place, cancel]);
        if let CsilRuleType::ServiceDef(service) = &mut input.csil_spec.rules[0].rule_type {
            service.wire_id = Some(3);
        }
        input
    }

    #[test]
    fn wire_ids_emitted_when_present() {
        let input = wire_id_input();
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(
            services.contains("const OrderServiceServiceWireID uint64 = 3"),
            "expected service ordinal const, got:\n{services}"
        );
        assert!(
            services.contains("const OrderServiceOpPlaceOrderWireID uint64 = 7"),
            "expected operation ordinal const, got:\n{services}"
        );
        // Operation without a wire-id contributes no const.
        assert!(
            !services.contains("CancelOrderWireID"),
            "operation without wire-id must not emit a const"
        );
    }

    #[test]
    fn wire_id_op_named_service_does_not_collide() {
        let mut place = make_op(
            "service",
            "Order",
            "Receipt",
            CsilServiceDirection::Unidirectional,
        );
        place.wire_id = Some(7);
        let mut input = input_with_service("OrderService", vec![place]);
        if let CsilRuleType::ServiceDef(service) = &mut input.csil_spec.rules[0].rule_type {
            service.wire_id = Some(3);
        }
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        // Op `service` becomes OrderServiceOpServiceWireID, distinct from the
        // service const OrderServiceServiceWireID, so Go won't redeclare a name.
        assert!(
            services.contains("const OrderServiceServiceWireID uint64 = 3"),
            "expected service ordinal const, got:\n{services}"
        );
        assert!(
            services.contains("const OrderServiceOpServiceWireID uint64 = 7"),
            "expected distinct op ordinal const, got:\n{services}"
        );
    }

    #[test]
    fn wire_ids_absent_when_unset() {
        let input = input_with_service(
            "OrderService",
            vec![make_op(
                "place-order",
                "Order",
                "Receipt",
                CsilServiceDirection::Unidirectional,
            )],
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(
            !services.contains("WireID"),
            "no wire-id output when service has no wire-id, got:\n{services}"
        );
    }

    // Build a channel (bidirectional) service carrying `@wire-id` ordinals so the
    // compact-router twin has something to dispatch on.
    fn wire_id_channel_input() -> WasmGeneratorInput {
        let mut play = make_op("play", "User", "User", CsilServiceDirection::Bidirectional);
        play.wire_id = Some(5);
        let mut input = input_with_service("Match", vec![play]);
        if let CsilRuleType::ServiceDef(service) = &mut input.csil_spec.rules[0].rule_type {
            service.wire_id = Some(1);
        }
        input
    }

    #[test]
    fn compact_router_emitted_for_wire_id_channel_service() {
        let input = wire_id_channel_input();
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Verbose router stays byte-identical alongside the compact twin.
        assert!(
            services.contains("func RouteMatchChannel(handlers Match, ctx context.Context, codec Codec, method string, data []byte) error"),
            "verbose router expected, got:\n{services}"
        );
        // Compact twin dispatches on the operation ordinal, not the wire name.
        assert!(
            services.contains("func RouteMatchChannelCompact(handlers Match, ctx context.Context, codec Codec, op uint64, data []byte) error"),
            "compact router expected, got:\n{services}"
        );
        assert!(
            services.contains("case 5:"),
            "compact router matches the op ordinal, got:\n{services}"
        );
        assert!(
            services.contains("return handlers.Play(ctx, msg)"),
            "compact router dispatches to the handler, got:\n{services}"
        );
        assert!(
            services.contains("unknown channel ordinal %d"),
            "compact router has an ordinal fallthrough, got:\n{services}"
        );
    }

    #[test]
    fn compact_router_absent_without_wire_id() {
        let input = input_with_service(
            "Match",
            vec![make_op(
                "play",
                "User",
                "User",
                CsilServiceDirection::Bidirectional,
            )],
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        // The verbose router survives; the compact twin must not appear.
        assert!(
            services.contains("func RouteMatchChannel("),
            "verbose router expected, got:\n{services}"
        );
        assert!(
            !services.contains("Compact"),
            "no compact router without wire-ids, got:\n{services}"
        );
    }
}
