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
    GeneratorConfig, GeneratorMetadata, WasmGeneratorInput, WasmGeneratorOutput,
    choice_arm_literal, wasm_interface::*,
};
use std::collections::{HashMap, HashSet};

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
        .map(php_class)
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
        out.push_str(&format!(
            "        $this->{prop} = array_key_exists({key}, $values) ? $values[{key}] : null;\n"
        ));
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
    out.push_str(
        "/** Raised when decoded CBOR does not match the declared CSIL shape (unknown enum\n * member, tagged-sum literal-arm mismatch, or malformed union envelope). */\nclass CodecException extends \\RuntimeException {}\n\n",
    );
    out.push_str("class Codec\n{\n");
    out.push_str(
        "    public static function encodeValue($value)\n    {\n        return CBOR::encode($value);\n    }\n\n    public static function decodeValue($bytes)\n    {\n        return CBOR::decode($bytes);\n    }\n\n    public static function toCborValue($value)\n    {\n        return $value;\n    }\n\n    public static function fromCborValue($value)\n    {\n        return $value;\n    }\n\n",
    );
    out.push_str(
        "    /** A literal-typed union/enum arm carries no shape of its own on the wire — the\n     * variant index (or the bare value itself for an enum) already selects it — so\n     * decode only needs to confirm the payload equals the declared literal. */\n    public static function expectLiteral($value, $expected)\n    {\n        if ($value !== $expected) {\n            throw new CodecException('csil cbor: literal mismatch, expected ' . var_export($expected, true) . ', got ' . var_export($value, true));\n        }\n        return $value;\n    }\n\n",
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
    let union_names: HashSet<String> = spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::TypeChoice(c) => Some((r.name.clone(), c)),
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(c)) => Some((r.name.clone(), c)),
            _ => None,
        })
        .filter(|(_, choices)| choice_is_union(choices))
        .map(|(name, _)| name)
        .collect();
    let enum_names: HashSet<String> = spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::TypeChoice(c) => Some((r.name.clone(), c)),
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(c)) => Some((r.name.clone(), c)),
            _ => None,
        })
        .filter(|(_, choices)| choice_is_enum(choices))
        .map(|(name, _)| name)
        .collect();

    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group)
            | CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                emit_group_codec(
                    &mut out,
                    &rule.name,
                    group,
                    &class_names,
                    &union_names,
                    &enum_names,
                );
            }
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => emit_choice_codec(
                &mut out,
                &rule.name,
                choices,
                &class_names,
                &union_names,
                &enum_names,
            ),
            CsilRuleType::TypeDef(expr) => emit_alias_codec(
                &mut out,
                &rule.name,
                expr,
                &class_names,
                &union_names,
                &enum_names,
            ),
            CsilRuleType::TypeChoice(choices) => emit_choice_codec(
                &mut out,
                &rule.name,
                choices,
                &class_names,
                &union_names,
                &enum_names,
            ),
            _ => {}
        }
    }
    out.push_str("}\n");
    out
}

/// Whether every variant of a named type-choice is a literal — ANY literal kind,
/// mixed kinds included (`"a" / 1`) — the CSIL wire contract's "ALL-literal choice"
/// (an enum): the wire value is the bare literal itself, self-discriminating by its
/// own CBOR major type + value, so a mixed-kind literal set needs no tag any more
/// than a uniform-kind one does. Delegates to the shared `csilgen_common::all_literal`
/// (see `crates/csilgen-common/src/choice.rs`'s module docs for the full contract)
/// rather than a local re-implementation. A prior local version additionally gated
/// each arm on a `literal_kind` helper restricted to text/int/float/bool, silently
/// excluding an all-bytes-literal (or a directly-API-constructed all-`Literal(Null)`)
/// choice from enum classification and misrouting it into `choice_is_union` instead
/// -- that gate had no technical basis (`php_literal` below already renders every
/// `CsilLiteralValue` kind, bytes included, into valid PHP, so nothing stopped this
/// generator emitting a correct bare-literal enum codec for those kinds too), and it
/// diverged from the Python generator's own reference `choice_arm_literal(c).is_some()`
/// check, which never had a kind gate. This mirrors the Go generator's
/// `choice_all_literal` and the empirically-observed Python/TypeScript generators'
/// behavior: `MixedLit = "a" / 1` rides bare in both (no dedicated codec — a plain
/// passthrough — because it's excluded from their union-def sets the same way an
/// enum is), and only diverges from a *uniform*-kind enum in that this generator
/// still emits a real membership-validated codec for it (`emit_enum_codec` already
/// handled mixed-kind literals fine before this change — it just collects each arm's
/// own literal, nothing in it required kind uniformity). `choice_arm_literal` sees
/// through a trailing control-operator wrapper on an arm.
///
/// A bare `null` choice arm written in real CSIL source never reaches here as a
/// literal at all: csilgen-core's `parse_primary_type` parses it as
/// `TypeExpression::Builtin("null")`, never `Literal(LiteralValue::Null)` — so
/// `choice_arm_literal` already returns `None` for it, same as any other open
/// builtin arm, and it falls through to `choice_is_union` as an ordinary "general"
/// arm. `CsilLiteralValue::Null` DOES remain constructible directly against this
/// generator's `WasmGeneratorInput` API (bypassing the parser); per the shared
/// contract it counts as a literal like any other kind, so an ALL-literal choice
/// built that way (every arm literal, one of them an explicit `Literal(Null)`)
/// now classifies as an enum too, rather than falling into `choice_is_union`'s
/// `has_null` guard (that guard only ever fires when a `Literal(Null)` arm sits
/// alongside a genuinely non-literal arm — see its own doc comment).
fn choice_is_enum(choices: &[CsilTypeExpression]) -> bool {
    csilgen_common::all_literal(choices)
}

/// Whether a named type-choice is a union per the CSIL wire contract: at least one
/// non-literal arm, and no literal `null` arm. An enum (see `choice_is_enum`) is
/// checked first and excluded, since its wire form is the bare literal, not
/// `[variant_index, value]`. Once an all-literal choice is excluded by that check,
/// the only way `has_null` can still fire below is a `Literal(Null)` arm sitting
/// alongside a genuinely non-literal arm (e.g. `text / Literal(Null)`) -- unreachable
/// from any choice real CSIL source produces (see `choice_is_enum`'s doc: a bare
/// `null` written in source is `Builtin("null")`, never `Literal(Null)`) but mirrors
/// the Python generator's own `python_union_defs`, which explicitly excludes a
/// literal `null` arm from union classification the same way — kept for parity with
/// that reference generator's source-level contract (not just its parsed-CSIL
/// output) and as a defensive guard against a `CsilLiteralValue::Null` arm built
/// directly via this generator's `WasmGeneratorInput` API rather than through the
/// parser: such a choice is neither a bare-literal enum nor a tagged-sum union and
/// keeps the pre-existing generic passthrough codec (see `emit_choice_codec`).
fn choice_is_union(choices: &[CsilTypeExpression]) -> bool {
    if choice_is_enum(choices) {
        return false;
    }
    let has_null = choices
        .iter()
        .any(|c| matches!(choice_arm_literal(c), Some(CsilLiteralValue::Null)));
    !has_null
}

/// Dispatch a named `TypeChoice`/`TypeDef(Choice)` rule to the right codec shape:
/// bare-literal enum, tagged-sum union, or (for anything neither — e.g. a nullable
/// non-enum choice) the pre-existing generic passthrough codec.
fn emit_choice_codec(
    out: &mut String,
    name: &str,
    choices: &[CsilTypeExpression],
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
    enums: &HashSet<String>,
) {
    if enums.contains(name) {
        let literals: Vec<CsilLiteralValue> = choices
            .iter()
            .filter_map(choice_arm_literal)
            .cloned()
            .collect();
        emit_enum_codec(out, name, &literals);
    } else if unions.contains(name) {
        emit_union_codec(out, name, choices, class_names, unions, enums);
    } else {
        emit_alias_codec(
            out,
            name,
            &CsilTypeExpression::Choice(choices.to_vec()),
            class_names,
            unions,
            enums,
        );
    }
}

/// Emit the bare-literal codec pair for an enum: encode is the identity (a validated
/// literal already is its own CBOR value, matching the Python generator's
/// `emit_enum_codec`); decode confirms the value is one of the declared members and
/// raises `CodecException` otherwise, rather than silently accepting an out-of-set
/// value.
fn emit_enum_codec(out: &mut String, name: &str, literals: &[CsilLiteralValue]) {
    let suffix = php_class(name);
    out.push_str(&format!(
        "    public static function encode{suffix}($value)\n    {{\n        return CBOR::encode(self::toCbor{suffix}($value));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static function decode{suffix}($bytes)\n    {{\n        return self::fromCbor{suffix}(CBOR::decode($bytes));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static function toCbor{suffix}($value)\n    {{\n        return $value;\n    }}\n\n"
    ));
    let members = literals
        .iter()
        .map(php_literal)
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "    public static function fromCbor{suffix}($value)\n    {{\n        static $csilMembers = array({members});\n        foreach ($csilMembers as $csilMember) {{\n            if ($value === $csilMember) {{\n                return $value;\n            }}\n        }}\n        throw new CodecException('csil cbor: unknown {name} value ' . var_export($value, true));\n    }}\n\n"
    ));
}

/// The PHP runtime-type dispatch key a union variant is checked against when
/// encoding (the value carries no tag at runtime, so the variant is found by its
/// shape). A record variant gets a distinct key per class — PHP `instanceof` really
/// distinguishes those — but every other CSIL shape narrows to one of PHP's few
/// runtime types, so e.g. `text` and `bytes` arms in the same union are NOT
/// distinguishable at runtime (both are a PHP `string`); a reference to another
/// named union/enum collapses to `"mixed"` for the same reason. This is an inherent
/// limit of dispatching on a dynamically-typed language's runtime shape rather than
/// an explicit tag, not something this codec can resolve unilaterally.
///
/// `null`/`nil` gets its OWN key (`is_null`), not the `_` catch-all's `"string"`:
/// a bare `null` choice arm always parses as `Builtin("null")` (see
/// `choice_is_enum`'s doc), so a choice like `"a" / 1 / null` reaches this function
/// with a real Builtin("null") general arm. Falling it into `"string"` would group
/// it with a text/bytes arm under an `is_string($value)` check that an actual PHP
/// `null` never satisfies — the null arm's branch would be dead code, and encoding
/// an actual `null` for that field would fall through every dispatch check and hit
/// the union's final "no matching variant" throw instead of encoding successfully.
fn php_union_dispatch_key(variant: &CsilTypeExpression, class_names: &HashSet<String>) -> String {
    match variant {
        CsilTypeExpression::Constrained { base_type, .. } => {
            php_union_dispatch_key(base_type, class_names)
        }
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "bool" => "bool".to_string(),
            "int" | "uint" | "nint" => "int".to_string(),
            "float" | "float16" | "float32" | "float64" | "double" => "float".to_string(),
            "null" | "nil" => "null".to_string(),
            _ => "string".to_string(),
        },
        CsilTypeExpression::Reference(name) if class_names.contains(name) => {
            format!("object:{}", php_class(name))
        }
        CsilTypeExpression::Array { .. }
        | CsilTypeExpression::Map { .. }
        | CsilTypeExpression::Tuple(_) => "array".to_string(),
        CsilTypeExpression::Literal(lit) => match lit {
            CsilLiteralValue::Bool(_) => "bool".to_string(),
            CsilLiteralValue::Integer(_) => "int".to_string(),
            CsilLiteralValue::Float(_) => "float".to_string(),
            CsilLiteralValue::Text(_) | CsilLiteralValue::Bytes(_) => "string".to_string(),
            CsilLiteralValue::Array(_) => "array".to_string(),
            // Excluded from every union by `choice_is_union`'s `has_null` guard
            // before this function is ever reached; kept only for match exhaustiveness.
            CsilLiteralValue::Null => "mixed".to_string(),
        },
        _ => "mixed".to_string(),
    }
}

/// The PHP boolean expression testing whether `var` has the runtime shape `key`
/// dispatches on. `"mixed"` has no structural test of its own — it is only ever
/// emitted as the final, unconditional fallback (see `emit_union_codec`).
fn php_dispatch_check(key: &str, var: &str) -> String {
    if let Some(class) = key.strip_prefix("object:") {
        format!("{var} instanceof {class}")
    } else {
        match key {
            "bool" => format!("is_bool({var})"),
            "int" => format!("is_int({var})"),
            "float" => format!("is_float({var})"),
            "string" => format!("is_string({var})"),
            "array" => format!("is_array({var})"),
            "null" => format!("{var} === null"),
            _ => "true".to_string(),
        }
    }
}

/// Emit the tagged-sum codec pair for a union: `toCbor<Suffix>` dispatches on the PHP
/// runtime shape of `$value` to find the variant index and returns `array($index,
/// $valueTree)`; `fromCbor<Suffix>` reads the index and reconstructs that variant.
/// A mixed union (e.g. `text / "pending" / "confirmed"`) groups its literal arms with
/// the general arm sharing their PHP dispatch type, checking the literals first by
/// value equality so they win over the general arm on collision — mirrors the
/// Go/Python generators' `emit_union_codec` grouping exactly.
/// The statement body (no enclosing function) encoding `value_expr` -- any
/// side-effect-free PHP expression naming the candidate value, referenced multiple
/// times -- to the locked `[variant_index, value]` array wire form, ending in an
/// unconditional `throw`. Shared by the named-union codec (`emit_union_codec`) and
/// the inline-choice codec (`to_cbor_expr`'s own `Choice` arm, wrapped in an IIFE
/// since an anonymous choice has no method to hang a `toCbor<Suffix>` off).
/// `choice_arm_literal` sees through a `.default`-style control-operator wrapper on
/// an arm so a constrained literal still wins its own declared index over a general
/// arm sharing its PHP dispatch type.
fn union_encode_body(
    variants: &[CsilTypeExpression],
    value_expr: &str,
    ctx: &str,
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
) -> String {
    let mut out = String::new();
    let mut type_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, variant) in variants.iter().enumerate() {
        let key = php_union_dispatch_key(variant, class_names);
        let entry = groups.entry(key.clone()).or_default();
        if entry.is_empty() {
            type_order.push(key);
        }
        entry.push(i);
    }
    // The unconditional "mixed" fallback (no structural test exists) must be tried
    // last, or it would shadow every real check that follows it.
    type_order.sort_by_key(|k| (k == "mixed") as u8);

    for key in &type_order {
        let idxs = &groups[key];
        let mut literal_idxs = Vec::new();
        let mut general_idx = None;
        for &i in idxs {
            if choice_arm_literal(&variants[i]).is_some() {
                literal_idxs.push(i);
            } else if general_idx.is_none() {
                // Two non-literal arms can share one PHP dispatch key (e.g. `text /
                // bytes` both narrow to `is_string`; two record-ref arms whose
                // classes aren't tracked, or two array-shaped arms, both narrow to
                // `is_array`) — PHP's runtime type is all this dispatch has to go
                // on, and it genuinely cannot tell those arms apart. The FIRST
                // declared arm in the group wins (lowest index) rather than the
                // last: encode must pick one deterministic, decodable variant, and
                // declaration order is the only signal the spec author actually
                // controls. Previously this unconditionally overwrote
                // `general_idx` on every non-literal arm, so the LAST arm silently
                // won instead — for `text / bytes` that meant `text` values
                // wire-encoded as variant 1 (`bytes`), corrupting cross-language
                // round-trips.
                general_idx = Some(i);
            }
        }
        if key == "mixed" {
            // No runtime check is possible; only reachable once every real check
            // above has failed, so this is the unconditional last resort.
            for i in literal_idxs {
                let lit = choice_arm_literal(&variants[i])
                    .expect("literal_idxs filtered to literal-carrying arms above");
                let lit_value = php_literal(lit);
                let enc = to_cbor_expr(value_expr, &variants[i], class_names, unions);
                out.push_str(&format!(
                    "if ({value_expr} === {lit_value}) {{\n    return array({i}, {enc});\n}}\n"
                ));
            }
            if let Some(gi) = general_idx {
                let enc = to_cbor_expr(value_expr, &variants[gi], class_names, unions);
                out.push_str(&format!("return array({gi}, {enc});\n"));
            }
            continue;
        }
        let check = php_dispatch_check(key, value_expr);
        if idxs.len() == 1 {
            let i = idxs[0];
            let enc = to_cbor_expr(value_expr, &variants[i], class_names, unions);
            out.push_str(&format!(
                "if ({check}) {{\n    return array({i}, {enc});\n}}\n"
            ));
            continue;
        }
        out.push_str(&format!("if ({check}) {{\n"));
        for i in literal_idxs {
            let lit = choice_arm_literal(&variants[i])
                .expect("literal_idxs filtered to literal-carrying arms above");
            let lit_value = php_literal(lit);
            let enc = to_cbor_expr(value_expr, &variants[i], class_names, unions);
            out.push_str(&format!(
                "    if ({value_expr} === {lit_value}) {{\n        return array({i}, {enc});\n    }}\n"
            ));
        }
        if let Some(gi) = general_idx {
            let enc = to_cbor_expr(value_expr, &variants[gi], class_names, unions);
            out.push_str(&format!("    return array({gi}, {enc});\n"));
        }
        out.push_str("}\n");
    }
    out.push_str(&format!(
        "throw new CodecException('csil cbor: value does not match any {ctx} variant');\n"
    ));
    out
}

/// The statement body decoding a `[variant_index, value]` array already split into
/// `idx_expr`/`val_expr` (both cheap/idempotent, referenced once each) back to the
/// arm at that index. Shared the same way `union_encode_body` is.
fn union_decode_body(
    variants: &[CsilTypeExpression],
    idx_expr: &str,
    val_expr: &str,
    ctx: &str,
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
    enums: &HashSet<String>,
) -> String {
    let mut out = String::new();
    for (i, variant) in variants.iter().enumerate() {
        let dec = from_cbor_expr(val_expr, variant, class_names, unions, enums);
        out.push_str(&format!(
            "if ({idx_expr} === {i}) {{\n    return {dec};\n}}\n"
        ));
    }
    out.push_str(&format!(
        "throw new CodecException('csil cbor: unknown {ctx} variant ' . var_export({idx_expr}, true));\n"
    ));
    out
}

/// Re-indent a `union_encode_body`/`union_decode_body` block (each line at column 0)
/// under a method body, prefixing every non-empty line with `indent`.
fn reindent(block: &str, indent: &str) -> String {
    // `split` (not `lines`, which drops a trailing empty segment) so a block ending
    // in `\n` keeps that trailing newline -- otherwise the next `push_str` lands on
    // the same source line as this block's last statement.
    block
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Emit the tagged-sum codec pair for a union: `toCbor<Suffix>` dispatches on the PHP
/// runtime shape of `$value` to find the variant index and returns `array($index,
/// $valueTree)`; `fromCbor<Suffix>` reads the index and reconstructs that variant.
/// A mixed union (e.g. `text / "pending" / "confirmed"`) groups its literal arms with
/// the general arm sharing their PHP dispatch type, checking the literals first by
/// value equality so they win over the general arm on collision — mirrors the
/// Go/Python generators' `emit_union_codec` grouping exactly.
fn emit_union_codec(
    out: &mut String,
    name: &str,
    variants: &[CsilTypeExpression],
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
    enums: &HashSet<String>,
) {
    let suffix = php_class(name);
    out.push_str(&format!(
        "    public static function encode{suffix}($value)\n    {{\n        return CBOR::encode(self::toCbor{suffix}($value));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static function decode{suffix}($bytes)\n    {{\n        return self::fromCbor{suffix}(CBOR::decode($bytes));\n    }}\n\n"
    ));

    out.push_str(&format!(
        "    public static function toCbor{suffix}($value)\n    {{\n"
    ));
    let enc_body = union_encode_body(variants, "$value", name, class_names, unions);
    out.push_str(&reindent(&enc_body, "        "));
    out.push_str("    }\n\n");

    out.push_str(&format!(
        "    public static function fromCbor{suffix}($value)\n    {{\n"
    ));
    out.push_str(&format!(
        "        if (!is_array($value) || count($value) !== 2) {{\n            throw new CodecException('csil cbor: {name} union expects a 2-element array');\n        }}\n"
    ));
    out.push_str("        $csilIdx = $value[0];\n        $csilVal = $value[1];\n");
    let dec_body = union_decode_body(
        variants,
        "$csilIdx",
        "$csilVal",
        name,
        class_names,
        unions,
        enums,
    );
    out.push_str(&reindent(&dec_body, "        "));
    out.push_str("    }\n\n");
}

fn emit_group_codec(
    out: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
    enums: &HashSet<String>,
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
                to_cbor_expr("$field", &entry.value_type, class_names, unions)
            ));
            out.push_str("        }\n");
        } else {
            out.push_str(&format!(
                "        $out[{key}] = {};\n",
                to_cbor_expr("$field", &entry.value_type, class_names, unions)
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
            from_cbor_expr(
                &format!("$value[{key}]"),
                &entry.value_type,
                class_names,
                unions,
                enums
            )
        ));
    }
    out.push_str("        ));\n    }\n\n");
}

fn emit_alias_codec(
    out: &mut String,
    name: &str,
    expr: &CsilTypeExpression,
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
    enums: &HashSet<String>,
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
        to_cbor_expr("$value", expr, class_names, unions)
    ));
    out.push_str(&format!(
        "    public static function fromCbor{suffix}($value)\n    {{\n        return {};\n    }}\n\n",
        from_cbor_expr("$value", expr, class_names, unions, enums)
    ));
}

fn to_cbor_expr(
    var: &str,
    expr: &CsilTypeExpression,
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
) -> String {
    match expr {
        CsilTypeExpression::Reference(name)
            if class_names.contains(name) || unions.contains(name) =>
        {
            format!("self::toCbor{}({var})", php_class(name))
        }
        CsilTypeExpression::Builtin(name) if name == "bytes" || name == "bstr" => {
            format!("CBOR::bytes({var})")
        }
        CsilTypeExpression::Array { element_type, .. } => format!(
            "array_map(function ($item) {{ return {}; }}, {var} === null ? array() : {var})",
            to_cbor_expr("$item", element_type, class_names, unions)
        ),
        CsilTypeExpression::Map { value, .. } => format!(
            "(function ($m) {{ $out = array(); foreach (($m === null ? array() : $m) as $k => $v) {{ $out[$k] = {}; }} return $out; }})({var})",
            to_cbor_expr("$v", value, class_names, unions)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => {
            to_cbor_expr(var, base_type, class_names, unions)
        }
        // A tuple is a positional CBOR array; its PHP value is a same-length array,
        // each position encoded per its own declared type (an absent optional
        // position rides as `null` in place, matching the locked fixed-length wire).
        CsilTypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let elem = format!("({var})[{i}]");
                    let enc = to_cbor_expr(&elem, &e.value_type, class_names, unions);
                    if is_optional(e) {
                        format!("(({elem}) === null ? null : ({enc}))")
                    } else {
                        enc
                    }
                })
                .collect();
            format!("array({})", parts.join(", "))
        }
        // An inline (anonymous) choice at a field / array-element / map-value /
        // tuple-element position gets exactly the same wire treatment a reference to
        // a named choice would: an ALL-literal choice (`choice_is_enum`, any literal
        // kind, mixed included — e.g. `"a" / 1`) rides bare (the PHP value already IS
        // the literal, so encoding it is the identity, matching a named enum
        // reference above), a mixed (non-all-literal) choice encodes to the locked
        // `[variant_index, value]` tagged sum via `union_encode_body` (the same
        // arm-grouping/precedence logic `emit_union_codec` uses for a named union),
        // wrapped in an immediately-invoked closure since an anonymous choice has no
        // method to hang a `toCbor<Suffix>` off. A choice that is neither (a literal
        // `null` arm built directly via this generator's API — see
        // `choice_is_union`'s `has_null` guard) keeps the pre-existing generic
        // passthrough.
        CsilTypeExpression::Choice(choices) => {
            if choice_is_enum(choices) {
                var.to_string()
            } else if choice_is_union(choices) {
                let body = union_encode_body(choices, "$value", "inline", class_names, unions);
                format!(
                    "(function ($value) {{\n{}}})({var})",
                    reindent(&body, "    ")
                )
            } else {
                var.to_string()
            }
        }
        CsilTypeExpression::Literal(lit) => php_literal(lit),
        _ => var.to_string(),
    }
}

/// The decode inverse of `to_cbor_expr`. Unlike encode, decode also routes through
/// `enums` (`fromCbor<Suffix>` validates CBOR-level membership in the declared
/// literal set) and gives a literal-typed field its own equality check
/// (`expectLiteral`) rather than trusting whatever value arrived at that slot — the
/// same "never silently accept a mistyped value" posture the union/enum codecs use.
fn from_cbor_expr(
    var: &str,
    expr: &CsilTypeExpression,
    class_names: &HashSet<String>,
    unions: &HashSet<String>,
    enums: &HashSet<String>,
) -> String {
    match expr {
        CsilTypeExpression::Reference(name)
            if class_names.contains(name) || unions.contains(name) || enums.contains(name) =>
        {
            format!("self::fromCbor{}({var})", php_class(name))
        }
        CsilTypeExpression::Array { element_type, .. } => format!(
            "array_map(function ($item) {{ return {}; }}, {var} === null ? array() : {var})",
            from_cbor_expr("$item", element_type, class_names, unions, enums)
        ),
        CsilTypeExpression::Map { value, .. } => format!(
            "(function ($m) {{ $out = array(); foreach (($m === null ? array() : $m) as $k => $v) {{ $out[$k] = {}; }} return $out; }})({var})",
            from_cbor_expr("$v", value, class_names, unions, enums)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => {
            from_cbor_expr(var, base_type, class_names, unions, enums)
        }
        // Positional tuple decode: rebuild the array from each position's own
        // declared type; an absent optional position decodes back to `null`.
        CsilTypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let elem = format!("({var})[{i}]");
                    let dec = from_cbor_expr(&elem, &e.value_type, class_names, unions, enums);
                    if is_optional(e) {
                        format!("(({elem}) === null ? null : ({dec}))")
                    } else {
                        dec
                    }
                })
                .collect();
            format!("array({})", parts.join(", "))
        }
        // An inline choice decodes with the same validation a named choice reference
        // gets: an ALL-literal choice (`choice_is_enum`, mixed kinds included)
        // validates CBOR-level membership in the declared literal set (mirrors
        // `emit_enum_codec`'s `fromCbor<Suffix>`), a mixed choice reads the
        // `[variant_index, value]` tagged sum via `union_decode_body`, both wrapped
        // in an IIFE for the same reason `to_cbor_expr`'s `Choice` arm is. A choice
        // that is neither (a literal `null` arm) keeps the pre-existing generic
        // passthrough.
        CsilTypeExpression::Choice(choices) => {
            if choice_is_enum(choices) {
                let members: Vec<String> = choices
                    .iter()
                    .filter_map(choice_arm_literal)
                    .map(php_literal)
                    .collect();
                format!(
                    "(function ($value) {{ foreach (array({}) as $csilMember) {{ if ($value === $csilMember) {{ return $value; }} }} throw new CodecException('csil cbor: unknown inline value ' . var_export($value, true)); }})({var})",
                    members.join(", ")
                )
            } else if choice_is_union(choices) {
                let body = union_decode_body(
                    choices,
                    "$csilIdx",
                    "$csilVal",
                    "inline",
                    class_names,
                    unions,
                    enums,
                );
                format!(
                    "(function ($value) {{\n    if (!is_array($value) || count($value) !== 2) {{\n        throw new CodecException('csil cbor: inline union expects a 2-element array');\n    }}\n    $csilIdx = $value[0];\n    $csilVal = $value[1];\n{}}})({var})",
                    reindent(&body, "    ")
                )
            } else {
                var.to_string()
            }
        }
        CsilTypeExpression::Literal(lit) => {
            format!("self::expectLiteral({var}, {})", php_literal(lit))
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
        "/**\n * The injected transport must expose call($service, $op, $payload) and return\n * the reply payload bytes. $service and $op are the CSIL names exactly as\n * written in the source and map verbatim onto the CSIL-RPC v1 envelope's\n * service/op fields.\n */\nclass {class}\n{{\n    private $transport;\n\n    public function __construct($transport)\n    {{\n        $this->transport = $transport;\n    }}\n\n"
    ));
    for op in &service.operations {
        if op.direction == CsilServiceDirection::Reverse {
            continue;
        }
        let method = php_method(&op.name);
        let in_suffix = codec_suffix(&op.input_type);
        let out_suffix = codec_suffix(&op.output_type);
        // The wire pair is the verbatim CSIL service/op names (cbor-wire-contract.md
        // "RPC call naming"): any case transform is lossy at the transport seam.
        out.push_str(&format!(
            "    public function {method}($request)\n    {{\n        $payload = Codec::encode{in_suffix}($request);\n        $reply = $this->transport->call({}, {}, $payload);\n        return Codec::decode{out_suffix}($reply);\n    }}\n\n",
            php_string(service_name),
            php_string(&op.name)
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
        "/**\n * Routes one decoded CSIL-RPC request for this service. $op is the wire op\n * name: the CSIL operation name exactly as written in the source (the\n * envelope's op field); the service is implied by which router is invoked.\n */\nclass {router}\n{{\n    private $handler;\n\n    public function __construct({iface} $handler)\n    {{\n        $this->handler = $handler;\n    }}\n\n    public function dispatch($op, $payload)\n    {{\n        switch ($op) {{\n"
    ));
    for op in &service.operations {
        if op.direction == CsilServiceDirection::Reverse {
            continue;
        }
        let in_suffix = codec_suffix(&op.input_type);
        let out_suffix = codec_suffix(&op.output_type);
        let method = php_method(&op.name);
        out.push_str(&format!(
            "            case {}:\n                $request = Codec::decode{in_suffix}($payload);\n                return Codec::encode{out_suffix}($this->handler->{method}($request));\n",
            php_string(&op.name)
        ));
    }
    out.push_str(
        "            default:\n                throw new \\InvalidArgumentException('unknown CSIL op: ' . $op);\n        }\n    }\n}\n\n",
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
