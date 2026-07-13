//! PHP code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target php` from `csilgen_php_generator.wasm`.
//! The emitted code targets PHP 7.2+: no typed properties, no union types, and no
//! dependencies beyond the hand-maintained `csilgen/transport` Composer package.

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSpecSerialized,
    CsilTypeExpression, GeneratedFile, GeneratedFiles, GenerationStats, GeneratorCapability,
    GeneratorConfig, GeneratorMetadata, WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};
use std::collections::HashSet;

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "php-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "PHP 7.x code generator".to_string(),
        target: "php".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: None,
    };
    write_json(&metadata) as *const u8
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
        Ok(output) => write_json(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn write_json<T: serde::Serialize>(value: &T) -> *mut u8 {
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

fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;
    let files = generate_php_code_from_serialized(&input.csil_spec, &input.config)?;
    let total_size = files.iter().map(|f| f.content.len()).sum();
    let files_generated = files.len();
    Ok(WasmGeneratorOutput {
        files,
        warnings: Vec::new(),
        stats: GenerationStats {
            files_generated,
            total_size_bytes: total_size,
            services_count: input.csil_spec.service_count,
            fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
            generation_time_ms: 0,
            peak_memory_bytes: None,
        },
    })
}

enum Surface {
    Server,
    Client,
    TypesOnly,
}

pub fn generate_php_code_from_serialized(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<GeneratedFiles, i32> {
    let surface = match config.target.as_str() {
        "php" | "php-server" => Surface::Server,
        "php-client" => Surface::Client,
        "php-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let namespace = php_namespace(config);
    let mut files = vec![
        GeneratedFile {
            path: "types.php".to_string(),
            content: generate_types_file(spec, &namespace),
        },
        GeneratedFile {
            path: "codec.php".to_string(),
            content: generate_codec_file(spec, &namespace),
        },
    ];

    let pkg_mode = emit_packages_includes(config, "php");
    if spec.service_count > 0 {
        let want_client = matches!(surface, Surface::Client)
            || (pkg_mode && !matches!(surface, Surface::TypesOnly));
        let want_server = matches!(surface, Surface::Server)
            || (pkg_mode && !matches!(surface, Surface::TypesOnly));
        if want_client {
            files.push(GeneratedFile {
                path: "client.php".to_string(),
                content: generate_client_file(spec, &namespace),
            });
        }
        if want_server {
            files.push(GeneratedFile {
                path: "server.php".to_string(),
                content: generate_server_file(spec, &namespace),
            });
        }
    }

    if pkg_mode {
        Ok(wrap_as_composer_package(files, config, &namespace))
    } else {
        Ok(files)
    }
}

fn emit_packages_includes(config: &GeneratorConfig, lang: &str) -> bool {
    config
        .options
        .get("emit_packages")
        .and_then(|value| value.as_array())
        .map(|array| array.iter().any(|element| element.as_str() == Some(lang)))
        .unwrap_or(false)
}

fn option_str<'a>(config: &'a GeneratorConfig, key: &str) -> Option<&'a str> {
    config
        .options
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn php_namespace(config: &GeneratorConfig) -> String {
    option_str(config, "php_namespace")
        .unwrap_or("Csilgen\\Generated")
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(|part| php_class(part))
        .collect::<Vec<_>>()
        .join("\\")
}

fn package_name(config: &GeneratorConfig) -> String {
    let raw = option_str(config, "package_name").unwrap_or("csilgen-client");
    raw.split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.to_case(Case::Kebab)
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
                .trim_matches('-')
                .to_string()
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn wrap_as_composer_package(
    files: GeneratedFiles,
    config: &GeneratorConfig,
    namespace: &str,
) -> GeneratedFiles {
    let name = package_name(config);
    let version = option_str(config, "package_version").unwrap_or("0.1.0");
    let composer_name = if name.contains('/') {
        name
    } else {
        format!("csilgen/{name}")
    };
    let mut out = vec![GeneratedFile {
        path: "composer.json".to_string(),
        content: format!(
            "{{\n  \"name\": \"{composer_name}\",\n  \"version\": \"{version}\",\n  \"type\": \"library\",\n  \"require\": {{\n    \"php\": \">=7.2\",\n    \"csilgen/transport\": \"*\"\n  }},\n  \"autoload\": {{\n    \"classmap\": [\"src/\"]\n  }}\n}}\n"
        ),
    }];
    for f in files {
        out.push(GeneratedFile {
            path: format!("src/{}", f.path),
            content: f.content,
        });
    }
    out.push(GeneratedFile {
        path: "genquickstart.md".to_string(),
        content: php_quickstart(&composer_name, namespace),
    });
    out
}

fn php_quickstart(package: &str, namespace: &str) -> String {
    format!(
        "# {package}\n\nInstall with Composer from a git checkout or Packagist once published.\n\n```bash\ncomposer require {package}\n```\n\nGenerated classes live under `{namespace}` and use `csilgen/transport` for CSIL-RPC, CSIL-Events, CSIL-Datagrams, and canonical CBOR bytes.\n"
    )
}

fn generate_types_file(spec: &CsilSpecSerialized, namespace: &str) -> String {
    let mut out = php_header(namespace);
    out.push_str("/** Generated CSIL value classes. */\n");
    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => emit_class(&mut out, &rule.name, group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                emit_class(&mut out, &rule.name, group)
            }
            _ => {}
        }
    }
    out
}

fn emit_class(out: &mut String, name: &str, group: &CsilGroupExpression) {
    let class = php_class(name);
    out.push_str(&format!("class {class}\n{{\n"));
    for entry in keyed_entries(group) {
        let prop = php_prop(&field_name(entry));
        out.push_str(&format!("    /** @var mixed */\n    public ${prop};\n\n"));
    }
    out.push_str("    /** @param array<string,mixed> $values */\n");
    out.push_str("    public function __construct(array $values = array())\n    {\n");
    for entry in keyed_entries(group) {
        let field = field_name(entry);
        let prop = php_prop(&field);
        let key = php_string(&field);
        if is_optional(entry) {
            out.push_str(&format!(
                "        $this->{prop} = array_key_exists({key}, $values) ? $values[{key}] : null;\n"
            ));
        } else {
            out.push_str(&format!(
                "        $this->{prop} = array_key_exists({key}, $values) ? $values[{key}] : null;\n"
            ));
        }
    }
    out.push_str("    }\n\n");
    out.push_str("    /** @return array<string,mixed> */\n");
    out.push_str("    public function toArray()\n    {\n        return array(\n");
    for entry in keyed_entries(group) {
        let field = field_name(entry);
        let prop = php_prop(&field);
        out.push_str(&format!(
            "            {} => $this->{prop},\n",
            php_string(&field)
        ));
    }
    out.push_str("        );\n    }\n}\n\n");
}

fn generate_codec_file(spec: &CsilSpecSerialized, namespace: &str) -> String {
    let mut out = php_header(namespace);
    out.push_str("use Csilgen\\Transport\\CBOR;\n\n");
    out.push_str("class Codec\n{\n");
    out.push_str(
        "    public static function encodeValue($value)\n    {\n        return CBOR::encode($value);\n    }\n\n    public static function decodeValue($bytes)\n    {\n        return CBOR::decode($bytes);\n    }\n\n    public static function toCborValue($value)\n    {\n        return $value;\n    }\n\n    public static function fromCborValue($value)\n    {\n        return $value;\n    }\n\n",
    );
    let class_names: HashSet<String> = spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) | CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => {
                Some(r.name.clone())
            }
            _ => None,
        })
        .collect();

    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group)
            | CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                emit_group_codec(&mut out, &rule.name, group, &class_names);
            }
            CsilRuleType::TypeDef(expr) => {
                emit_alias_codec(&mut out, &rule.name, expr, &class_names)
            }
            CsilRuleType::TypeChoice(choices) => emit_alias_codec(
                &mut out,
                &rule.name,
                &CsilTypeExpression::Choice(choices.clone()),
                &class_names,
            ),
            _ => {}
        }
    }
    out.push_str("}\n");
    out
}

fn emit_group_codec(
    out: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    class_names: &HashSet<String>,
) {
    let suffix = php_class(name);
    out.push_str(&format!(
        "    public static function encode{suffix}($value)\n    {{\n        return CBOR::encode(self::toCbor{suffix}($value));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static function decode{suffix}($bytes)\n    {{\n        return self::fromCbor{suffix}(CBOR::decode($bytes));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static function toCbor{suffix}($value)\n    {{\n        $out = array();\n"
    ));
    for entry in keyed_entries(group) {
        let field = field_name(entry);
        let prop = php_prop(&field);
        let key = php_string(&field);
        out.push_str(&format!(
            "        $field = $value instanceof {suffix} ? $value->{prop} : (is_array($value) && array_key_exists({key}, $value) ? $value[{key}] : null);\n"
        ));
        if is_optional(entry) {
            out.push_str("        if ($field !== null) {\n");
            out.push_str(&format!(
                "            $out[{key}] = {};\n",
                to_cbor_expr("$field", &entry.value_type, class_names)
            ));
            out.push_str("        }\n");
        } else {
            out.push_str(&format!(
                "        $out[{key}] = {};\n",
                to_cbor_expr("$field", &entry.value_type, class_names)
            ));
        }
    }
    out.push_str("        return $out;\n    }\n\n");
    out.push_str(&format!(
        "    public static function fromCbor{suffix}($value)\n    {{\n        return new {suffix}(array(\n"
    ));
    for entry in keyed_entries(group) {
        let field = field_name(entry);
        let key = php_string(&field);
        out.push_str(&format!(
            "            {key} => array_key_exists({key}, $value) ? {} : null,\n",
            from_cbor_expr(&format!("$value[{key}]"), &entry.value_type, class_names)
        ));
    }
    out.push_str("        ));\n    }\n\n");
}

fn emit_alias_codec(
    out: &mut String,
    name: &str,
    expr: &CsilTypeExpression,
    class_names: &HashSet<String>,
) {
    let suffix = php_class(name);
    out.push_str(&format!(
        "    public static function encode{suffix}($value)\n    {{\n        return CBOR::encode(self::toCbor{suffix}($value));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static function decode{suffix}($bytes)\n    {{\n        return self::fromCbor{suffix}(CBOR::decode($bytes));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static function toCbor{suffix}($value)\n    {{\n        return {};\n    }}\n\n",
        to_cbor_expr("$value", expr, class_names)
    ));
    out.push_str(&format!(
        "    public static function fromCbor{suffix}($value)\n    {{\n        return {};\n    }}\n\n",
        from_cbor_expr("$value", expr, class_names)
    ));
}

fn to_cbor_expr(var: &str, expr: &CsilTypeExpression, class_names: &HashSet<String>) -> String {
    match expr {
        CsilTypeExpression::Reference(name) if class_names.contains(name) => {
            format!("self::toCbor{}({var})", php_class(name))
        }
        CsilTypeExpression::Builtin(name) if name == "bytes" || name == "bstr" => {
            format!("CBOR::bytes({var})")
        }
        CsilTypeExpression::Array { element_type, .. } => format!(
            "array_map(function ($item) {{ return {}; }}, $var === null ? array() : $var)",
            to_cbor_expr("$item", element_type, class_names)
        ),
        CsilTypeExpression::Map { value, .. } => format!(
            "(function ($m) {{ $out = array(); foreach (($m === null ? array() : $m) as $k => $v) {{ $out[$k] = {}; }} return $out; }})({var})",
            to_cbor_expr("$v", value, class_names)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => {
            to_cbor_expr(var, base_type, class_names)
        }
        CsilTypeExpression::Choice(_) => var.to_string(),
        CsilTypeExpression::Literal(lit) => php_literal(lit),
        _ => var.to_string(),
    }
}

fn from_cbor_expr(var: &str, expr: &CsilTypeExpression, class_names: &HashSet<String>) -> String {
    match expr {
        CsilTypeExpression::Reference(name) if class_names.contains(name) => {
            format!("self::fromCbor{}({var})", php_class(name))
        }
        CsilTypeExpression::Array { element_type, .. } => format!(
            "array_map(function ($item) {{ return {}; }}, $var === null ? array() : $var)",
            from_cbor_expr("$item", element_type, class_names)
        ),
        CsilTypeExpression::Map { value, .. } => format!(
            "(function ($m) {{ $out = array(); foreach (($m === null ? array() : $m) as $k => $v) {{ $out[$k] = {}; }} return $out; }})({var})",
            from_cbor_expr("$v", value, class_names)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => {
            from_cbor_expr(var, base_type, class_names)
        }
        _ => var.to_string(),
    }
}

fn generate_client_file(spec: &CsilSpecSerialized, namespace: &str) -> String {
    let mut out = php_header(namespace);
    out.push_str("class ClientError extends \\RuntimeException {}\n\n");
    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_class(&mut out, &rule.name, service);
        }
    }
    out
}

fn emit_client_class(out: &mut String, service_name: &str, service: &CsilServiceDefinition) {
    let class = format!("{}Client", service_base(service_name));
    out.push_str(&format!(
        "class {class}\n{{\n    private $transport;\n\n    public function __construct($transport)\n    {{\n        $this->transport = $transport;\n    }}\n\n"
    ));
    for op in &service.operations {
        if op.direction == CsilServiceDirection::Reverse {
            continue;
        }
        let method = php_method(&op.name);
        let wire = wire_method_name(service_name, &op.name);
        let in_suffix = codec_suffix(&op.input_type);
        let out_suffix = codec_suffix(&op.output_type);
        out.push_str(&format!(
            "    public function {method}($request)\n    {{\n        $payload = Codec::encode{in_suffix}($request);\n        $reply = $this->transport->call({}, $payload);\n        return Codec::decode{out_suffix}($reply);\n    }}\n\n",
            php_string(&wire)
        ));
    }
    out.push_str("}\n\n");
}

fn generate_server_file(spec: &CsilSpecSerialized, namespace: &str) -> String {
    let mut out = php_header(namespace);
    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_server_class(&mut out, &rule.name, service);
        }
    }
    out
}

fn emit_server_class(out: &mut String, service_name: &str, service: &CsilServiceDefinition) {
    let base = service_base(service_name);
    let iface = format!("{base}Handler");
    let router = format!("{base}Router");
    out.push_str(&format!("interface {iface}\n{{\n"));
    for op in &service.operations {
        if op.direction == CsilServiceDirection::Reverse {
            continue;
        }
        out.push_str(&format!(
            "    public function {}($request);\n",
            php_method(&op.name)
        ));
    }
    out.push_str("}\n\n");
    out.push_str(&format!(
        "class {router}\n{{\n    private $handler;\n\n    public function __construct({iface} $handler)\n    {{\n        $this->handler = $handler;\n    }}\n\n    public function dispatch($method, $payload)\n    {{\n        switch ($method) {{\n"
    ));
    for op in &service.operations {
        if op.direction == CsilServiceDirection::Reverse {
            continue;
        }
        let wire = wire_method_name(service_name, &op.name);
        let in_suffix = codec_suffix(&op.input_type);
        let out_suffix = codec_suffix(&op.output_type);
        let method = php_method(&op.name);
        out.push_str(&format!(
            "            case {}:\n                $request = Codec::decode{in_suffix}($payload);\n                return Codec::encode{out_suffix}($this->handler->{method}($request));\n",
            php_string(&wire)
        ));
    }
    out.push_str(
        "            default:\n                throw new \\InvalidArgumentException('unknown CSIL method: ' . $method);\n        }\n    }\n}\n\n",
    );
}

fn codec_suffix(expr: &CsilTypeExpression) -> String {
    match expr {
        CsilTypeExpression::Reference(name) => php_class(name),
        _ => "Value".to_string(),
    }
}

fn keyed_entries(group: &CsilGroupExpression) -> impl Iterator<Item = &CsilGroupEntry> {
    group.entries.iter().filter(|entry| entry.key.is_some())
}

fn field_name(entry: &CsilGroupEntry) -> String {
    match entry.key.as_ref().expect("keyed entry") {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Integer(n)) => n.to_string(),
        CsilGroupKey::Literal(_) => "field".to_string(),
        CsilGroupKey::Type(_) => "value".to_string(),
    }
}

fn is_optional(entry: &CsilGroupEntry) -> bool {
    matches!(entry.occurrence, Some(CsilOccurrence::Optional))
}

fn php_header(namespace: &str) -> String {
    format!("<?php\n\nnamespace {namespace};\n\n")
}

fn php_class(name: &str) -> String {
    let mut out = name.to_case(Case::Pascal);
    if out.is_empty()
        || !out
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        out = format!("Csil{out}");
    }
    if PHP_RESERVED.contains(&out.to_ascii_lowercase().as_str()) {
        out.push_str("Value");
    }
    out
}

fn php_prop(name: &str) -> String {
    let mut out = name.to_case(Case::Camel);
    if out.is_empty()
        || !out
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        out = format!("field{out}");
    }
    if PHP_RESERVED.contains(&out.to_ascii_lowercase().as_str()) {
        out.push('_');
    }
    out
}

fn php_method(name: &str) -> String {
    php_prop(name)
}

fn service_base(name: &str) -> String {
    php_class(name)
        .strip_suffix("Service")
        .unwrap_or(&php_class(name))
        .to_string()
}

fn wire_method_name(service: &str, op: &str) -> String {
    format!(
        "{}/{}",
        service.to_case(Case::Kebab),
        op.to_case(Case::Kebab)
    )
}

fn php_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn php_literal(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(n) => n.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => php_string(s),
        CsilLiteralValue::Bytes(bytes) => {
            let hex = bytes
                .iter()
                .map(|b| format!("\\x{b:02x}"))
                .collect::<String>();
            php_string(&hex)
        }
        CsilLiteralValue::Bool(true) => "true".to_string(),
        CsilLiteralValue::Bool(false) => "false".to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        CsilLiteralValue::Array(items) => format!(
            "array({})",
            items.iter().map(php_literal).collect::<Vec<_>>().join(", ")
        ),
    }
}

const PHP_RESERVED: &[&str] = &[
    "abstract",
    "and",
    "array",
    "as",
    "break",
    "callable",
    "case",
    "catch",
    "class",
    "clone",
    "const",
    "continue",
    "declare",
    "default",
    "die",
    "do",
    "echo",
    "else",
    "elseif",
    "empty",
    "enddeclare",
    "endfor",
    "endforeach",
    "endif",
    "endswitch",
    "endwhile",
    "eval",
    "exit",
    "extends",
    "final",
    "finally",
    "fn",
    "for",
    "foreach",
    "function",
    "global",
    "goto",
    "if",
    "implements",
    "include",
    "include_once",
    "instanceof",
    "insteadof",
    "interface",
    "isset",
    "list",
    "namespace",
    "new",
    "or",
    "print",
    "private",
    "protected",
    "public",
    "require",
    "require_once",
    "return",
    "static",
    "switch",
    "throw",
    "trait",
    "try",
    "unset",
    "use",
    "var",
    "while",
    "xor",
    "yield",
];
