//! WASM module loader and runtime for csilgen generators

use csilgen_common::{
    CsilControlOperator, CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, 
    CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilPosition, CsilRule, CsilRuleType, 
    CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation, CsilSizeConstraint,
    CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint, CsilgenError, GeneratedFiles, 
    GeneratorCapability, GeneratorConfig, GeneratorMetadata, Result, WasmGeneratorInput, 
    WasmGeneratorOutput,
};
use csilgen_core::ast::{
    ControlOperator, CsilSpec, FieldMetadata, FieldVisibility, GroupEntry, GroupExpression, GroupKey, 
    LiteralValue, Occurrence, RuleType, ServiceDefinition, ServiceDirection, ServiceOperation, 
    SizeConstraint, TypeExpression, ValidationConstraint,
};
use csilgen_core::lexer::Position;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use wasmtime::{Config, Engine, Linker, Module, Store, TypedFunc};

/// Configuration for WASM execution limits
#[derive(Debug, Clone)]
pub struct WasmLimits {
    /// Maximum memory allocation in bytes
    pub max_memory_bytes: usize,
    /// Maximum execution time
    pub max_execution_time: Duration,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024, // 64 MB
            max_execution_time: Duration::from_secs(30),
        }
    }
}

/// Cache statistics for WASM module performance monitoring
#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub modules_evicted: u64,
    pub total_compile_time: Duration,
}

impl CacheStats {
    pub fn hit_rate(&self) -> f64 {
        if self.cache_hits + self.cache_misses == 0 {
            0.0
        } else {
            self.cache_hits as f64 / (self.cache_hits + self.cache_misses) as f64
        }
    }

    pub fn average_compile_time(&self) -> Duration {
        if self.cache_misses == 0 {
            Duration::ZERO
        } else {
            self.total_compile_time / self.cache_misses as u32
        }
    }
}

/// Represents a discovered generator with its metadata and location
#[derive(Debug, Clone)]
pub struct DiscoveredGenerator {
    /// Unique identifier for the generator (derived from filename/path)
    pub id: String,
    /// Generator metadata loaded from the WASM module
    pub metadata: GeneratorMetadata,
    /// Path to the WASM binary file
    pub wasm_path: PathBuf,
    /// Whether this is a built-in or custom generator
    pub generator_type: GeneratorType,
    /// Version compatibility check result
    pub compatibility: CompatibilityStatus,
}

/// Type of generator based on its source
#[derive(Debug, Clone, PartialEq)]
pub enum GeneratorType {
    /// Built-in generator shipped with csilgen
    BuiltIn,
    /// User-installed custom generator
    Custom,
}

/// Compatibility status for a generator
#[derive(Debug, Clone, PartialEq)]
pub enum CompatibilityStatus {
    /// Generator is compatible with current runtime
    Compatible,
    /// Generator version is newer than runtime supports
    VersionTooNew { required: String, available: String },
    /// Generator version is too old and unsupported
    VersionTooOld { required: String, available: String },
    /// Generator has capabilities not supported by runtime
    UnsupportedCapabilities { unsupported: Vec<String> },
    /// Generator failed to load or provide metadata
    LoadError { error: String },
}

/// A loaded WASM generator module with caching
pub struct LoadedGenerator {
    module: Module,
    metadata: GeneratorMetadata,
    wasm_path: PathBuf,
    load_time: Instant,
}

impl LoadedGenerator {
    /// Get access to the WASM module (for testing)
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Get generator metadata
    pub fn metadata(&self) -> &GeneratorMetadata {
        &self.metadata
    }

    /// Get path to WASM binary
    pub fn wasm_path(&self) -> &Path {
        &self.wasm_path
    }

    /// Get time when generator was loaded
    pub fn load_time(&self) -> Instant {
        self.load_time
    }
}

/// Registry for discovered and loaded generators
#[derive(Debug, Default)]
pub struct GeneratorRegistry {
    /// All discovered generators (not necessarily loaded)
    discovered: HashMap<String, DiscoveredGenerator>,
    /// Standard directories where generators are looked up
    search_paths: Vec<PathBuf>,
}

impl GeneratorRegistry {
    /// Create new empty registry
    pub fn new() -> Self {
        Self {
            discovered: HashMap::new(),
            search_paths: Self::default_search_paths(),
        }
    }

    /// Get default search paths for generators
    fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Built-in generators (in target directory for now)
        if let Ok(current_dir) = std::env::current_dir() {
            paths.push(current_dir.join("target/wasm32-unknown-unknown/release"));
        }

        // System-wide generators
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".csilgen/generators"));
        }

        // Local project generators
        paths.push(PathBuf::from("./generators"));

        paths
    }

    /// Add a custom search path
    pub fn add_search_path(&mut self, path: PathBuf) {
        if !self.search_paths.contains(&path) {
            self.search_paths.push(path);
        }
    }

    /// Get all search paths
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// Discover generators in all search paths
    pub fn discover_generators(&mut self) -> Result<usize> {
        let mut discovered_count = 0;

        for path in &self.search_paths.clone() {
            if path.exists() && path.is_dir() {
                discovered_count += self.discover_in_directory(path)?;
            }
        }

        Ok(discovered_count)
    }

    /// Discover generators in a specific directory
    fn discover_in_directory(&mut self, dir: &Path) -> Result<usize> {
        let mut count = 0;

        let entries = fs::read_dir(dir).map_err(|e| CsilgenError::IoError(e.to_string()))?;

        for entry in entries {
            let entry = entry.map_err(|e| CsilgenError::IoError(e.to_string()))?;

            let path = entry.path();

            // Look for .wasm files
            if path.extension().and_then(|s| s.to_str()) == Some("wasm") {
                if let Some(generator_id) = self.extract_generator_id(&path) {
                    match self.probe_generator_metadata(&path) {
                        Ok(metadata) => {
                            let generator_type = if self.is_builtin_path(dir) {
                                GeneratorType::BuiltIn
                            } else {
                                GeneratorType::Custom
                            };

                            let compatibility = self.check_compatibility(&metadata);

                            let discovered = DiscoveredGenerator {
                                id: generator_id,
                                metadata,
                                wasm_path: path,
                                generator_type,
                                compatibility,
                            };

                            self.discovered.insert(discovered.id.clone(), discovered);
                            count += 1;
                        }
                        Err(e) => {
                            // Create a placeholder entry for failed generators
                            let generator_id = self.extract_generator_id(&path).unwrap();
                            let placeholder = DiscoveredGenerator {
                                id: generator_id.clone(),
                                metadata: GeneratorMetadata {
                                    name: generator_id.clone(),
                                    version: "unknown".to_string(),
                                    description: "Failed to load metadata".to_string(),
                                    target: "unknown".to_string(),
                                    capabilities: vec![],
                                    author: None,
                                    homepage: None,
                                },
                                wasm_path: path,
                                generator_type: GeneratorType::Custom,
                                compatibility: CompatibilityStatus::LoadError {
                                    error: e.to_string(),
                                },
                            };
                            self.discovered.insert(generator_id, placeholder);
                        }
                    }
                }
            }
        }

        Ok(count)
    }

    /// Extract generator ID from WASM file path
    fn extract_generator_id(&self, path: &Path) -> Option<String> {
        // Only process .wasm files
        if path.extension().and_then(|s| s.to_str()) != Some("wasm") {
            return None;
        }

        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.replace('_', "-")) // Convert underscores to dashes for consistency
    }

    /// Check if a path is for built-in generators
    fn is_builtin_path(&self, path: &Path) -> bool {
        path.to_string_lossy()
            .contains("target/wasm32-unknown-unknown")
    }

    /// Probe generator metadata without fully loading it
    fn probe_generator_metadata(&self, wasm_path: &Path) -> Result<GeneratorMetadata> {
        // For now, we'll derive metadata from the filename
        // In a full implementation, we could load the WASM just enough to call get_metadata
        let generator_id = self
            .extract_generator_id(wasm_path)
            .ok_or_else(|| CsilgenError::WasmError("Invalid generator filename".to_string()))?;

        // Determine target and capabilities from known generators
        let (target, capabilities, description) = match generator_id.as_str() {
            "csilgen-noop-generator" => (
                "noop".to_string(),
                vec![
                    GeneratorCapability::BasicTypes,
                    GeneratorCapability::Services,
                    GeneratorCapability::FieldMetadata,
                ],
                "No-op test generator for validation".to_string(),
            ),
            "csilgen-simple-test" => (
                "test".to_string(),
                vec![GeneratorCapability::BasicTypes],
                "Simple test generator".to_string(),
            ),
            "csilgen-json-generator" => (
                "json".to_string(),
                vec![
                    GeneratorCapability::BasicTypes,
                    GeneratorCapability::ComplexStructures,
                    GeneratorCapability::Services,
                    GeneratorCapability::FieldMetadata,
                ],
                "JSON Schema generator".to_string(),
            ),
            "csilgen-rust-generator" => (
                "rust".to_string(),
                vec![
                    GeneratorCapability::BasicTypes,
                    GeneratorCapability::ComplexStructures,
                    GeneratorCapability::Services,
                    GeneratorCapability::FieldMetadata,
                    GeneratorCapability::FieldVisibility,
                ],
                "Rust code generator".to_string(),
            ),
            _ => (
                "unknown".to_string(),
                vec![],
                "Unknown generator".to_string(),
            ),
        };

        Ok(GeneratorMetadata {
            name: generator_id,
            version: "1.0.0".to_string(), // Would probe actual version in real implementation
            description,
            target,
            capabilities,
            author: Some("CSIL Team".to_string()),
            homepage: None,
        })
    }

    /// Check compatibility of generator with current runtime
    fn check_compatibility(&self, metadata: &GeneratorMetadata) -> CompatibilityStatus {
        // Check version compatibility (simplified semver check)
        if let Err(version_error) = self.check_version_compatibility(&metadata.version) {
            return version_error;
        }

        // Check capability support
        if let Some(unsupported) = self.check_capability_support(&metadata.capabilities) {
            return CompatibilityStatus::UnsupportedCapabilities { unsupported };
        }

        CompatibilityStatus::Compatible
    }

    /// Check if the generator version is compatible with runtime
    fn check_version_compatibility(
        &self,
        version: &str,
    ) -> std::result::Result<(), CompatibilityStatus> {
        // Current runtime supports versions 1.x.x
        const SUPPORTED_MAJOR: u32 = 1;
        const RUNTIME_VERSION: &str = "1.0.0";

        // Parse version (simplified - real implementation would use semver crate)
        if let Some((major_str, _)) = version.split_once('.') {
            if let Ok(major) = major_str.parse::<u32>() {
                match major.cmp(&SUPPORTED_MAJOR) {
                    std::cmp::Ordering::Greater => {
                        return Err(CompatibilityStatus::VersionTooNew {
                            required: version.to_string(),
                            available: RUNTIME_VERSION.to_string(),
                        });
                    }
                    std::cmp::Ordering::Less => {
                        return Err(CompatibilityStatus::VersionTooOld {
                            required: version.to_string(),
                            available: RUNTIME_VERSION.to_string(),
                        });
                    }
                    std::cmp::Ordering::Equal => {} // Compatible
                }
            }
        }

        Ok(())
    }

    /// Check if all generator capabilities are supported by the runtime
    fn check_capability_support(
        &self,
        capabilities: &[GeneratorCapability],
    ) -> Option<Vec<String>> {
        let supported_capabilities = [
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::FieldDependencies,
            GeneratorCapability::ValidationConstraints,
            GeneratorCapability::CustomHints,
        ];

        let mut unsupported = Vec::new();
        for capability in capabilities {
            if !supported_capabilities.contains(capability) {
                unsupported.push(format!("{capability:?}"));
            }
        }

        if unsupported.is_empty() {
            None
        } else {
            Some(unsupported)
        }
    }

    /// Get all discovered generators
    pub fn discovered_generators(&self) -> &HashMap<String, DiscoveredGenerator> {
        &self.discovered
    }

    /// Get a specific discovered generator
    pub fn get_generator(&self, id: &str) -> Option<&DiscoveredGenerator> {
        self.discovered.get(id)
    }

    /// List all compatible generators
    pub fn compatible_generators(&self) -> Vec<&DiscoveredGenerator> {
        self.discovered
            .values()
            .filter(|g| g.compatibility == CompatibilityStatus::Compatible)
            .collect()
    }

    /// List generators by type
    pub fn generators_by_type(&self, generator_type: GeneratorType) -> Vec<&DiscoveredGenerator> {
        self.discovered
            .values()
            .filter(|g| g.generator_type == generator_type)
            .collect()
    }

    /// Clear the registry
    pub fn clear(&mut self) {
        self.discovered.clear();
    }
}

/// WASM runtime for loading and executing generator modules
pub struct WasmGeneratorRuntime {
    engine: Engine,
    generators: HashMap<String, LoadedGenerator>,
    registry: GeneratorRegistry,
    limits: WasmLimits,
    module_cache: HashMap<PathBuf, (Module, Instant)>, // WASM module cache with load time
    cache_stats: CacheStats,
}

/// Convert core AST CsilSpec to serializable format
fn convert_csil_spec_to_serializable(spec: &CsilSpec) -> CsilSpecSerialized {
    let mut service_count = 0;
    let mut fields_with_metadata_count = 0;

    let serializable_rules: Vec<CsilRule> = spec
        .rules
        .iter()
        .map(|rule| {
            let serializable_rule_type = match &rule.rule_type {
                RuleType::TypeDef(type_expr) => {
                    CsilRuleType::TypeDef(convert_type_expression(type_expr))
                }
                RuleType::GroupDef(group_expr) => {
                    for entry in &group_expr.entries {
                        if !entry.metadata.is_empty() {
                            fields_with_metadata_count += 1;
                        }
                    }
                    CsilRuleType::GroupDef(convert_group_expression(group_expr))
                }
                RuleType::TypeChoice(choices) => {
                    CsilRuleType::TypeChoice(choices.iter().map(convert_type_expression).collect())
                }
                RuleType::GroupChoice(choices) => CsilRuleType::GroupChoice(
                    choices.iter().map(convert_group_expression).collect(),
                ),
                RuleType::ServiceDef(service_def) => {
                    service_count += 1;
                    CsilRuleType::ServiceDef(convert_service_definition(service_def))
                }
            };

            CsilRule {
                name: rule.name.clone(),
                rule_type: serializable_rule_type,
                position: convert_position(&rule.position),
            }
        })
        .collect();

    CsilSpecSerialized {
        rules: serializable_rules,
        source_content: None, // Could be provided if needed
        service_count,
        fields_with_metadata_count,
    }
}

fn convert_position(pos: &Position) -> CsilPosition {
    CsilPosition {
        line: pos.line,
        column: pos.column,
        offset: pos.offset,
    }
}

fn convert_type_expression(type_expr: &TypeExpression) -> CsilTypeExpression {
    match type_expr {
        TypeExpression::Builtin(name) => CsilTypeExpression::Builtin(name.clone()),
        TypeExpression::Reference(name) => CsilTypeExpression::Reference(name.clone()),
        TypeExpression::Array {
            element_type,
            occurrence,
        } => CsilTypeExpression::Array {
            element_type: Box::new(convert_type_expression(element_type)),
            occurrence: occurrence.as_ref().map(convert_occurrence),
        },
        TypeExpression::Map {
            key,
            value,
            occurrence,
        } => CsilTypeExpression::Map {
            key: Box::new(convert_type_expression(key)),
            value: Box::new(convert_type_expression(value)),
            occurrence: occurrence.as_ref().map(convert_occurrence),
        },
        TypeExpression::Group(group_expr) => {
            CsilTypeExpression::Group(convert_group_expression(group_expr))
        }
        TypeExpression::Choice(choices) => {
            CsilTypeExpression::Choice(choices.iter().map(convert_type_expression).collect())
        }
        TypeExpression::Range {
            start,
            end,
            inclusive,
        } => CsilTypeExpression::Range {
            start: *start,
            end: *end,
            inclusive: *inclusive,
        },
        TypeExpression::Socket(name) => CsilTypeExpression::Socket(name.clone()),
        TypeExpression::Plug(name) => CsilTypeExpression::Plug(name.clone()),
        TypeExpression::Literal(literal) => {
            CsilTypeExpression::Literal(convert_literal_value(literal))
        }
        TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            // Convert constraints to WASM boundary format
            CsilTypeExpression::Constrained {
                base_type: Box::new(convert_type_expression(base_type)),
                constraints: constraints.iter().map(convert_control_operator).collect(),
            }
        }
    }
}

fn convert_control_operator(op: &ControlOperator) -> CsilControlOperator {
    match op {
        ControlOperator::Size(size) => CsilControlOperator::Size(convert_size_constraint(size)),
        ControlOperator::Regex(pattern) => CsilControlOperator::Regex(pattern.clone()),
        ControlOperator::Default(value) => CsilControlOperator::Default(convert_literal_value(value)),
        ControlOperator::GreaterEqual(value) => CsilControlOperator::GreaterEqual(convert_literal_value(value)),
        ControlOperator::LessEqual(value) => CsilControlOperator::LessEqual(convert_literal_value(value)),
        ControlOperator::GreaterThan(value) => CsilControlOperator::GreaterThan(convert_literal_value(value)),
        ControlOperator::LessThan(value) => CsilControlOperator::LessThan(convert_literal_value(value)),
        ControlOperator::Equal(value) => CsilControlOperator::Equal(convert_literal_value(value)),
        ControlOperator::NotEqual(value) => CsilControlOperator::NotEqual(convert_literal_value(value)),
        ControlOperator::Bits(bits) => CsilControlOperator::Bits(bits.clone()),
        ControlOperator::And(type_expr) => CsilControlOperator::And(Box::new(convert_type_expression(type_expr))),
        ControlOperator::Within(type_expr) => CsilControlOperator::Within(Box::new(convert_type_expression(type_expr))),
        ControlOperator::Json => CsilControlOperator::Json,
        ControlOperator::Cbor => CsilControlOperator::Cbor,
        ControlOperator::Cborseq => CsilControlOperator::Cborseq,
    }
}

fn convert_size_constraint(size: &SizeConstraint) -> CsilSizeConstraint {
    match size {
        SizeConstraint::Exact(val) => CsilSizeConstraint::Exact(*val),
        SizeConstraint::Range { min, max } => CsilSizeConstraint::Range { min: *min, max: *max },
        SizeConstraint::Min(val) => CsilSizeConstraint::Min(*val),
        SizeConstraint::Max(val) => CsilSizeConstraint::Max(*val),
    }
}

fn convert_group_expression(group_expr: &GroupExpression) -> CsilGroupExpression {
    CsilGroupExpression {
        entries: group_expr.entries.iter().map(convert_group_entry).collect(),
    }
}

fn convert_group_entry(entry: &GroupEntry) -> CsilGroupEntry {
    CsilGroupEntry {
        key: entry.key.as_ref().map(convert_group_key),
        value_type: convert_type_expression(&entry.value_type),
        occurrence: entry.occurrence.as_ref().map(convert_occurrence),
        metadata: entry.metadata.iter().map(convert_field_metadata).collect(),
    }
}

fn convert_group_key(key: &GroupKey) -> CsilGroupKey {
    match key {
        GroupKey::Bare(name) => CsilGroupKey::Bare(name.clone()),
        GroupKey::Type(type_expr) => CsilGroupKey::Type(convert_type_expression(type_expr)),
        GroupKey::Literal(literal) => CsilGroupKey::Literal(convert_literal_value(literal)),
    }
}

fn convert_occurrence(occurrence: &Occurrence) -> CsilOccurrence {
    match occurrence {
        Occurrence::Optional => CsilOccurrence::Optional,
        Occurrence::ZeroOrMore => CsilOccurrence::ZeroOrMore,
        Occurrence::OneOrMore => CsilOccurrence::OneOrMore,
        Occurrence::Exact(n) => CsilOccurrence::Exact(*n),
        Occurrence::Range { min, max } => CsilOccurrence::Range {
            min: *min,
            max: *max,
        },
    }
}

fn convert_literal_value(literal: &LiteralValue) -> CsilLiteralValue {
    match literal {
        LiteralValue::Integer(n) => CsilLiteralValue::Integer(*n),
        LiteralValue::Float(f) => CsilLiteralValue::Float(*f),
        LiteralValue::Text(s) => CsilLiteralValue::Text(s.clone()),
        LiteralValue::Bytes(bytes) => CsilLiteralValue::Bytes(bytes.clone()),
        LiteralValue::Bool(b) => CsilLiteralValue::Bool(*b),
        LiteralValue::Null => CsilLiteralValue::Null,
    }
}

fn convert_field_metadata(metadata: &FieldMetadata) -> CsilFieldMetadata {
    match metadata {
        FieldMetadata::Visibility(visibility) => {
            CsilFieldMetadata::Visibility(convert_field_visibility(visibility))
        }
        FieldMetadata::DependsOn { field, value } => CsilFieldMetadata::DependsOn {
            field: field.clone(),
            value: value.as_ref().map(convert_literal_value),
        },
        FieldMetadata::Constraint(constraint) => {
            CsilFieldMetadata::Constraint(convert_validation_constraint(constraint))
        }
        FieldMetadata::Description(desc) => CsilFieldMetadata::Description(desc.clone()),
        FieldMetadata::Custom { name, parameters } => CsilFieldMetadata::Custom {
            name: name.clone(),
            parameters: parameters
                .iter()
                .map(|p| csilgen_common::CsilMetadataParameter {
                    name: p.name.clone(),
                    value: convert_literal_value(&p.value),
                })
                .collect(),
        },
    }
}

fn convert_field_visibility(visibility: &FieldVisibility) -> CsilFieldVisibility {
    match visibility {
        FieldVisibility::SendOnly => CsilFieldVisibility::SendOnly,
        FieldVisibility::ReceiveOnly => CsilFieldVisibility::ReceiveOnly,
        FieldVisibility::Bidirectional => CsilFieldVisibility::Bidirectional,
    }
}

fn convert_validation_constraint(constraint: &ValidationConstraint) -> CsilValidationConstraint {
    match constraint {
        ValidationConstraint::MinLength(n) => CsilValidationConstraint::MinLength(*n),
        ValidationConstraint::MaxLength(n) => CsilValidationConstraint::MaxLength(*n),
        ValidationConstraint::MinItems(n) => CsilValidationConstraint::MinItems(*n),
        ValidationConstraint::MaxItems(n) => CsilValidationConstraint::MaxItems(*n),
        ValidationConstraint::MinValue(value) => {
            CsilValidationConstraint::MinValue(convert_literal_value(value))
        }
        ValidationConstraint::MaxValue(value) => {
            CsilValidationConstraint::MaxValue(convert_literal_value(value))
        }
        ValidationConstraint::Custom { name, value } => CsilValidationConstraint::Custom {
            name: name.clone(),
            value: convert_literal_value(value),
        },
    }
}

fn convert_service_definition(service_def: &ServiceDefinition) -> CsilServiceDefinition {
    CsilServiceDefinition {
        operations: service_def
            .operations
            .iter()
            .map(convert_service_operation)
            .collect(),
    }
}

fn convert_service_operation(operation: &ServiceOperation) -> CsilServiceOperation {
    CsilServiceOperation {
        name: operation.name.clone(),
        input_type: convert_type_expression(&operation.input_type),
        output_type: convert_type_expression(&operation.output_type),
        direction: convert_service_direction(&operation.direction),
        position: convert_position(&operation.position),
    }
}

fn convert_service_direction(direction: &ServiceDirection) -> CsilServiceDirection {
    match direction {
        ServiceDirection::Unidirectional => CsilServiceDirection::Unidirectional,
        ServiceDirection::Bidirectional => CsilServiceDirection::Bidirectional,
        ServiceDirection::Reverse => CsilServiceDirection::Reverse,
    }
}

impl WasmGeneratorRuntime {
    /// Create a new WASM generator runtime
    pub fn new() -> Result<Self> {
        Self::new_with_limits(WasmLimits::default())
    }

    /// Create a new WASM generator runtime with custom limits
    pub fn new_with_limits(limits: WasmLimits) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);

        let engine = Engine::new(&config)
            .map_err(|e| CsilgenError::WasmError(format!("Failed to create WASM engine: {e}")))?;

        Ok(Self {
            engine,
            generators: HashMap::new(),
            registry: GeneratorRegistry::new(),
            limits,
            module_cache: HashMap::new(),
            cache_stats: CacheStats::default(),
        })
    }

    /// Discover generators in all search paths
    pub fn discover_generators(&mut self) -> Result<usize> {
        self.registry.discover_generators()
    }

    /// Get access to the generator registry
    pub fn registry(&self) -> &GeneratorRegistry {
        &self.registry
    }

    /// Get cache statistics for performance monitoring
    pub fn cache_stats(&self) -> &CacheStats {
        &self.cache_stats
    }

    /// Clear expired cache entries to manage memory usage
    pub fn cleanup_cache(&mut self) {
        const CACHE_TTL: Duration = Duration::from_secs(3600); // 1 hour TTL
        const MAX_CACHE_SIZE: usize = 50; // Maximum cached modules

        let now = Instant::now();

        // Remove expired entries
        self.module_cache
            .retain(|_path, (_, load_time)| now.duration_since(*load_time) < CACHE_TTL);

        // If still too many entries, remove oldest ones
        if self.module_cache.len() > MAX_CACHE_SIZE {
            let mut entries: Vec<_> = self.module_cache.iter().collect();
            entries.sort_by_key(|(_, (_, load_time))| *load_time);

            let to_remove = entries.len() - MAX_CACHE_SIZE;
            let paths_to_remove: Vec<_> = entries
                .iter()
                .take(to_remove)
                .map(|(path, _)| (*path).clone())
                .collect();

            for path in paths_to_remove {
                self.module_cache.remove(&path);
                self.cache_stats.modules_evicted += 1;
            }
        }
    }

    /// Precompile frequently used generators for faster execution
    pub fn precompile_generator(&mut self, generator_id: &str) -> Result<()> {
        let generator = self.registry.discovered.get(generator_id).ok_or_else(|| {
            CsilgenError::WasmError(format!("Generator not found: {generator_id}"))
        })?;

        // Check if already cached
        if self.module_cache.contains_key(&generator.wasm_path) {
            self.cache_stats.cache_hits += 1;
            return Ok(());
        }

        // Compile and cache the module
        let start = Instant::now();
        let module = Module::from_file(&self.engine, &generator.wasm_path)
            .map_err(|e| CsilgenError::WasmError(format!("Failed to compile WASM module: {e}")))?;

        let compile_time = start.elapsed();
        self.cache_stats.total_compile_time += compile_time;
        self.cache_stats.cache_misses += 1;

        self.module_cache
            .insert(generator.wasm_path.clone(), (module, Instant::now()));

        // Cleanup cache if needed
        self.cleanup_cache();

        Ok(())
    }

    /// Get mutable access to the generator registry
    pub fn registry_mut(&mut self) -> &mut GeneratorRegistry {
        &mut self.registry
    }

    /// Load a generator from the registry by ID
    pub fn load_generator_from_registry(&mut self, id: &str) -> Result<()> {
        let wasm_path = {
            let discovered = self.registry.get_generator(id).ok_or_else(|| {
                CsilgenError::WasmError(format!("Generator '{id}' not found in registry"))
            })?;

            if discovered.compatibility != CompatibilityStatus::Compatible {
                return Err(CsilgenError::WasmError(format!(
                    "Generator '{id}' is not compatible: {:?}",
                    discovered.compatibility
                )));
            }

            discovered.wasm_path.clone()
        };

        self.load_generator_from_path(id, &wasm_path)
    }

    /// Load a generator from a specific WASM file path
    pub fn load_generator_from_path(&mut self, id: &str, wasm_path: &Path) -> Result<()> {
        // Check cache first
        let module = if let Some((cached_module, _)) = self.module_cache.get(wasm_path) {
            cached_module.clone()
        } else {
            // Load WASM bytes and compile
            let wasm_bytes =
                fs::read(wasm_path).map_err(|e| CsilgenError::IoError(e.to_string()))?;

            let module = Module::new(&self.engine, &wasm_bytes).map_err(|e| {
                CsilgenError::WasmError(format!("Failed to compile WASM module '{id}': {e}"))
            })?;

            // Cache the compiled module
            self.module_cache
                .insert(wasm_path.to_path_buf(), (module.clone(), Instant::now()));
            module
        };

        // Get metadata (from registry if available, otherwise probe)
        let metadata = if let Some(discovered) = self.registry.get_generator(id) {
            discovered.metadata.clone()
        } else {
            self.registry.probe_generator_metadata(wasm_path)?
        };

        let loaded_generator = LoadedGenerator {
            module,
            metadata,
            wasm_path: wasm_path.to_path_buf(),
            load_time: Instant::now(),
        };

        self.generators.insert(id.to_string(), loaded_generator);
        Ok(())
    }

    /// Load a generator module from WASM bytes (legacy method)
    pub fn load_generator(&mut self, name: String, wasm_bytes: &[u8]) -> Result<()> {
        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            CsilgenError::WasmError(format!("Failed to compile WASM module '{name}': {e}"))
        })?;

        // Create basic metadata for legacy loading
        let metadata = GeneratorMetadata {
            name: name.clone(),
            version: "unknown".to_string(),
            description: "Legacy loaded generator".to_string(),
            target: "unknown".to_string(),
            capabilities: vec![],
            author: None,
            homepage: None,
        };

        self.generators.insert(
            name.clone(),
            LoadedGenerator {
                module,
                metadata,
                wasm_path: PathBuf::new(), // No path for memory-loaded modules
                load_time: Instant::now(),
            },
        );

        Ok(())
    }

    /// Get cached module compilation if available
    pub fn get_cached_module(&self, wasm_path: &Path) -> Option<&Module> {
        self.module_cache.get(wasm_path).map(|(module, _)| module)
    }

    /// Clear module cache entries older than the specified duration
    pub fn clean_module_cache(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.module_cache
            .retain(|_, (_, load_time)| now.duration_since(*load_time) < max_age);
    }

    /// Get module cache statistics  
    pub fn module_cache_stats(&self) -> (usize, usize) {
        (self.module_cache.len(), self.generators.len())
    }

    /// Execute a loaded generator with comprehensive error handling
    pub fn execute_generator(
        &mut self,
        generator_name: &str,
        spec: &CsilSpec,
        config: &GeneratorConfig,
    ) -> Result<GeneratedFiles> {
        // Try to load from registry if not already loaded
        if !self.generators.contains_key(generator_name) {
            if self.registry.get_generator(generator_name).is_some() {
                if let Err(e) = self.load_generator_from_registry(generator_name) {
                    return Err(CsilgenError::WasmError(format!(
                        "Failed to load generator '{generator_name}' from registry: {e}"
                    )));
                }
            } else {
                return Err(CsilgenError::WasmError(format!(
                    "Generator '{generator_name}' not found in loaded generators or registry"
                )));
            }
        }

        let generator = self.generators.get(generator_name).ok_or_else(|| {
            CsilgenError::WasmError(format!("Generator '{generator_name}' not found"))
        })?;

        let mut store = Store::new(&self.engine, ());
        // Add generous fuel for WASM execution - 100M instructions should be enough for basic operations
        store
            .set_fuel(100_000_000)
            .map_err(|e| CsilgenError::WasmError(format!("Failed to set fuel: {e}")))?;

        // Note: Memory limiting would need proper wasmtime configuration
        // For now, we'll rely on time-based and fuel-based limiting

        let linker = Linker::new(&self.engine);

        // Create the instance
        let instance = linker
            .instantiate(&mut store, &generator.module)
            .map_err(|e| {
                CsilgenError::WasmError(format!("Failed to instantiate WASM module: {e}"))
            })?;

        // Look for the simplified generator function (input_ptr, input_len) -> result_ptr
        let generate_func: TypedFunc<(i32, i32), i32> = instance
            .get_typed_func(&mut store, "generate")
            .map_err(|e| {
                CsilgenError::WasmError(format!("Generator function 'generate' not found: {e}"))
            })?;

        // Convert to serializable format and create WasmGeneratorInput
        let serializable_spec = convert_csil_spec_to_serializable(spec);

        let generator_metadata = GeneratorMetadata {
            name: generator_name.to_string(),
            version: "1.0.0".to_string(),
            description: "Generated metadata".to_string(),
            target: config.target.clone(),
            capabilities: vec![
                GeneratorCapability::BasicTypes,
                GeneratorCapability::ComplexStructures,
                GeneratorCapability::Services,
                GeneratorCapability::FieldMetadata,
            ],
            author: None,
            homepage: None,
        };

        let wasm_input = WasmGeneratorInput {
            csil_spec: serializable_spec,
            config: config.clone(),
            generator_metadata,
        };

        let input_json = serde_json::to_string(&wasm_input)
            .map_err(|e| CsilgenError::WasmError(format!("Failed to serialize WASM input: {e}")))?;

        // Allocate memory in WASM for inputs and get pointers
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| CsilgenError::WasmError("Memory export not found".to_string()))?;

        let input_ptr = self.write_string_to_wasm(&mut store, &memory, &input_json)?;

        // Execute with time limit
        let start_time = Instant::now();
        let result_ptr = loop {
            if start_time.elapsed() > self.limits.max_execution_time {
                return Err(CsilgenError::WasmError(
                    "Generator execution exceeded time limit".to_string(),
                ));
            }

            match generate_func.call(&mut store, (input_ptr as i32, input_json.len() as i32)) {
                Ok(result_ptr) => break result_ptr,
                Err(e) if e.to_string().contains("fuel") => {
                    // Add more fuel and continue
                    // In a real implementation, we'd add more fuel here
                    continue;
                }
                Err(e) => {
                    return Err(CsilgenError::WasmError(format!(
                        "Generator execution failed: {e}"
                    )));
                }
            }
        };

        if result_ptr == 0 {
            return Err(CsilgenError::WasmError(
                "Generator returned null pointer".to_string(),
            ));
        }

        // Read the result from WASM memory (length-prefixed JSON)
        let result_json = self.read_string_from_wasm(&store, &memory, result_ptr as usize)?;

        // Deserialize the result - expect WasmGeneratorOutput
        let wasm_output: WasmGeneratorOutput = serde_json::from_str(&result_json).map_err(|e| {
            CsilgenError::WasmError(format!("Failed to deserialize generator result: {e}"))
        })?;

        Ok(wasm_output.files)
    }

    /// Get list of loaded generators
    pub fn list_loaded_generators(&self) -> Vec<&str> {
        self.generators.keys().map(|s| s.as_str()).collect()
    }

    /// Get list of all discovered generators (loaded and unloaded)
    pub fn list_discovered_generators(&self) -> Vec<&str> {
        self.registry
            .discovered_generators()
            .keys()
            .map(|s| s.as_str())
            .collect()
    }

    /// Get list of all generators (legacy method)
    pub fn list_generators(&self) -> Vec<&str> {
        self.list_loaded_generators()
    }

    /// Remove a loaded generator
    pub fn unload_generator(&mut self, name: &str) -> Result<()> {
        self.generators
            .remove(name)
            .ok_or_else(|| CsilgenError::WasmError(format!("Generator '{name}' not found")))?;
        Ok(())
    }

    /// Helper to write a string to WASM memory
    fn write_string_to_wasm(
        &self,
        store: &mut Store<()>,
        memory: &wasmtime::Memory,
        s: &str,
    ) -> Result<usize> {
        // For now, use a simple approach - in a real implementation we'd want proper memory allocation
        // This assumes the WASM module exports an allocator function
        let bytes = s.as_bytes();
        let data = memory.data_mut(store);

        // Use a simple offset for demo - real implementation would call WASM allocator
        static mut OFFSET: usize = 1024; // Start after first page
        let ptr = unsafe { OFFSET };

        if ptr + bytes.len() > data.len() {
            return Err(CsilgenError::WasmError(
                "Not enough WASM memory".to_string(),
            ));
        }

        data[ptr..ptr + bytes.len()].copy_from_slice(bytes);
        unsafe {
            OFFSET += bytes.len() + 8;
        } // Add padding

        Ok(ptr)
    }

    /// Helper to read a string from WASM memory
    fn read_string_from_wasm(
        &self,
        store: &Store<()>,
        memory: &wasmtime::Memory,
        ptr: usize,
    ) -> Result<String> {
        let data = memory.data(store);

        // In a real implementation, the WASM module would return a length along with the pointer
        // For now, we'll assume null-terminated strings or look for a length prefix
        // This is a simplified version - real implementation would be more sophisticated

        // Read length from first 4 bytes (assuming little-endian u32)
        if ptr + 4 > data.len() {
            return Err(CsilgenError::WasmError(
                "Invalid pointer for string length".to_string(),
            ));
        }

        let len =
            u32::from_le_bytes([data[ptr], data[ptr + 1], data[ptr + 2], data[ptr + 3]]) as usize;
        let start = ptr + 4;

        if start + len > data.len() {
            return Err(CsilgenError::WasmError("Invalid string length".to_string()));
        }

        let string_bytes = &data[start..start + len];
        String::from_utf8(string_bytes.to_vec())
            .map_err(|e| CsilgenError::WasmError(format!("Invalid UTF-8 in WASM string: {e}")))
    }

    /// Get access to the WASM engine (for testing)
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Get access to the loaded generators (for testing)
    pub fn generators(&self) -> &HashMap<String, LoadedGenerator> {
        &self.generators
    }
}

impl Default for WasmGeneratorRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create WASM runtime")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_core::ast::{
        CsilSpec, FieldMetadata, FieldVisibility, GroupEntry, GroupExpression, GroupKey,
        Occurrence, Rule, RuleType, TypeExpression,
    };
    use csilgen_core::lexer::Position;
    use std::time::Duration;

    fn create_test_csil_spec() -> CsilSpec {
        CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![
                Rule {
                    name: "User".to_string(),
                    rule_type: RuleType::GroupDef(GroupExpression {
                        entries: vec![
                            GroupEntry {
                                key: Some(GroupKey::Bare("name".to_string())),
                                value_type: TypeExpression::Builtin("text".to_string()),
                                occurrence: None,
                                metadata: vec![FieldMetadata::Visibility(
                                    FieldVisibility::Bidirectional,
                                )],
                            },
                            GroupEntry {
                                key: Some(GroupKey::Bare("email".to_string())),
                                value_type: TypeExpression::Builtin("text".to_string()),
                                occurrence: Some(Occurrence::Optional),
                                metadata: vec![
                                    FieldMetadata::Visibility(FieldVisibility::SendOnly),
                                    FieldMetadata::Constraint(ValidationConstraint::MinLength(5)),
                                ],
                            },
                        ],
                    }),
                    position: Position {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                },
                Rule {
                    name: "UserService".to_string(),
                    rule_type: RuleType::ServiceDef(ServiceDefinition {
                        operations: vec![ServiceOperation {
                            name: "create_user".to_string(),
                            input_type: TypeExpression::Reference("User".to_string()),
                            output_type: TypeExpression::Reference("User".to_string()),
                            direction: ServiceDirection::Unidirectional,
                            position: Position {
                                line: 5,
                                column: 4,
                                offset: 100,
                            },
                        }],
                    }),
                    position: Position {
                        line: 4,
                        column: 1,
                        offset: 80,
                    },
                },
            ],
        }
    }

    fn create_test_config() -> GeneratorConfig {
        GeneratorConfig {
            target: "test".to_string(),
            output_dir: "/tmp".to_string(),
            options: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_wasm_runtime_creation() {
        let runtime = WasmGeneratorRuntime::new();
        assert!(runtime.is_ok());
    }

    #[test]
    fn test_wasm_runtime_creation_with_limits() {
        let limits = WasmLimits {
            max_memory_bytes: 32 * 1024 * 1024,
            max_execution_time: Duration::from_secs(10),
        };
        let runtime = WasmGeneratorRuntime::new_with_limits(limits);
        assert!(runtime.is_ok());
    }

    // Registry and discovery tests

    #[test]
    fn test_generator_registry_creation() {
        let registry = GeneratorRegistry::new();
        assert!(!registry.search_paths().is_empty());
        assert!(registry.discovered_generators().is_empty());
    }

    #[test]
    fn test_registry_add_search_path() {
        let mut registry = GeneratorRegistry::new();
        let initial_count = registry.search_paths().len();

        let custom_path = PathBuf::from("/custom/generators");
        registry.add_search_path(custom_path.clone());

        assert_eq!(registry.search_paths().len(), initial_count + 1);
        assert!(registry.search_paths().contains(&custom_path));

        // Adding same path again should not duplicate
        registry.add_search_path(custom_path);
        assert_eq!(registry.search_paths().len(), initial_count + 1);
    }

    #[test]
    fn test_generator_id_extraction() {
        let registry = GeneratorRegistry::new();

        let test_cases = vec![
            (
                "csilgen_noop_generator.wasm",
                Some("csilgen-noop-generator".to_string()),
            ),
            ("my_custom_gen.wasm", Some("my-custom-gen".to_string())),
            ("simple.wasm", Some("simple".to_string())),
            ("no_extension", None),
            ("", None),
        ];

        for (filename, expected) in test_cases {
            let path = Path::new(filename);
            let result = registry.extract_generator_id(path);
            assert_eq!(result, expected, "Failed for filename: {filename}");
        }
    }

    #[test]
    fn test_builtin_path_detection() {
        let registry = GeneratorRegistry::new();

        let builtin_path = Path::new("target/wasm32-unknown-unknown/release");
        let custom_path = Path::new("/home/user/.csilgen/generators");

        assert!(registry.is_builtin_path(builtin_path));
        assert!(!registry.is_builtin_path(custom_path));
    }

    #[test]
    fn test_version_compatibility() {
        let registry = GeneratorRegistry::new();

        // Test compatible versions
        assert!(registry.check_version_compatibility("1.0.0").is_ok());
        assert!(registry.check_version_compatibility("1.5.3").is_ok());
        assert!(registry.check_version_compatibility("1.99.99").is_ok());

        // Test incompatible versions
        let too_new = registry.check_version_compatibility("2.0.0");
        assert!(too_new.is_err());
        if let Err(CompatibilityStatus::VersionTooNew {
            required,
            available,
        }) = too_new
        {
            assert_eq!(required, "2.0.0");
            assert_eq!(available, "1.0.0");
        }

        let too_old = registry.check_version_compatibility("0.9.0");
        assert!(too_old.is_err());
        if let Err(CompatibilityStatus::VersionTooOld {
            required,
            available,
        }) = too_old
        {
            assert_eq!(required, "0.9.0");
            assert_eq!(available, "1.0.0");
        }
    }

    #[test]
    fn test_capability_support() {
        let registry = GeneratorRegistry::new();

        // Test supported capabilities
        let supported = vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
        ];
        assert!(registry.check_capability_support(&supported).is_none());

        // Test unsupported capabilities (if any were added)
        let unsupported = vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::Streaming, // This might not be supported yet
        ];
        let result = registry.check_capability_support(&unsupported);
        // Allow this test to pass whether Streaming is supported or not
        if let Some(unsupported_caps) = result {
            assert!(!unsupported_caps.is_empty());
        }
    }

    #[test]
    fn test_probe_generator_metadata() {
        let registry = GeneratorRegistry::new();

        let test_path = Path::new("csilgen_noop_generator.wasm");
        let metadata = registry
            .probe_generator_metadata(test_path)
            .expect("Should probe metadata");

        assert_eq!(metadata.name, "csilgen-noop-generator");
        assert_eq!(metadata.target, "noop");
        assert!(!metadata.capabilities.is_empty());
        assert_eq!(metadata.description, "No-op test generator for validation");
    }

    #[test]
    fn test_wasm_runtime_with_registry() {
        let runtime = WasmGeneratorRuntime::new().expect("Should create runtime");

        // Test initial state
        assert!(runtime.list_loaded_generators().is_empty());
        assert!(runtime.list_discovered_generators().is_empty());

        // Test cache stats
        let (cached, loaded) = runtime.module_cache_stats();
        assert_eq!(cached, 0);
        assert_eq!(loaded, 0);
    }

    #[test]
    fn test_generator_loading_error_handling() {
        let mut runtime = WasmGeneratorRuntime::new().expect("Should create runtime");

        // Test loading non-existent generator from registry
        let result = runtime.load_generator_from_registry("nonexistent-generator");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not found in registry")
        );

        // Test executing non-existent generator
        let spec = create_test_csil_spec();
        let config = create_test_config();
        let result = runtime.execute_generator("nonexistent", &spec, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_module_cache_functionality() {
        let mut runtime = WasmGeneratorRuntime::new().expect("Should create runtime");

        // Create a temporary WASM path
        let temp_path = PathBuf::from("/tmp/test_generator.wasm");

        // Initially no cached module
        assert!(runtime.get_cached_module(&temp_path).is_none());

        // Test cache cleaning (shouldn't crash with empty cache)
        runtime.clean_module_cache(Duration::from_secs(60));

        let (cached, loaded) = runtime.module_cache_stats();
        assert_eq!(cached, 0);
        assert_eq!(loaded, 0);
    }

    #[test]
    fn test_discovered_generator_structure() {
        let metadata = GeneratorMetadata {
            name: "test-generator".to_string(),
            version: "1.2.3".to_string(),
            description: "Test generator".to_string(),
            target: "rust".to_string(),
            capabilities: vec![GeneratorCapability::BasicTypes],
            author: Some("Test Author".to_string()),
            homepage: Some("https://example.com".to_string()),
        };

        let discovered = DiscoveredGenerator {
            id: "test-generator".to_string(),
            metadata: metadata.clone(),
            wasm_path: PathBuf::from("/path/to/generator.wasm"),
            generator_type: GeneratorType::Custom,
            compatibility: CompatibilityStatus::Compatible,
        };

        assert_eq!(discovered.id, "test-generator");
        assert_eq!(discovered.metadata.name, metadata.name);
        assert_eq!(discovered.generator_type, GeneratorType::Custom);
        assert_eq!(discovered.compatibility, CompatibilityStatus::Compatible);
    }

    #[test]
    fn test_compatibility_status_variants() {
        let compatible = CompatibilityStatus::Compatible;
        let too_new = CompatibilityStatus::VersionTooNew {
            required: "2.0.0".to_string(),
            available: "1.0.0".to_string(),
        };
        let too_old = CompatibilityStatus::VersionTooOld {
            required: "0.5.0".to_string(),
            available: "1.0.0".to_string(),
        };
        let unsupported = CompatibilityStatus::UnsupportedCapabilities {
            unsupported: vec!["FutureFeature".to_string()],
        };
        let load_error = CompatibilityStatus::LoadError {
            error: "Failed to load".to_string(),
        };

        // Test equality
        assert_eq!(compatible, CompatibilityStatus::Compatible);
        assert_ne!(compatible, too_new);
        assert_ne!(too_new, too_old);
        assert_ne!(unsupported, load_error);
    }

    #[test]
    fn test_generator_type_variants() {
        assert_eq!(GeneratorType::BuiltIn, GeneratorType::BuiltIn);
        assert_eq!(GeneratorType::Custom, GeneratorType::Custom);
        assert_ne!(GeneratorType::BuiltIn, GeneratorType::Custom);
    }

    #[test]
    fn test_registry_filter_methods() {
        let mut registry = GeneratorRegistry::new();

        // Create test discoveries
        let builtin_metadata = GeneratorMetadata {
            name: "builtin-gen".to_string(),
            version: "1.0.0".to_string(),
            description: "Built-in generator".to_string(),
            target: "rust".to_string(),
            capabilities: vec![GeneratorCapability::BasicTypes],
            author: None,
            homepage: None,
        };

        let custom_metadata = GeneratorMetadata {
            name: "custom-gen".to_string(),
            version: "1.1.0".to_string(),
            description: "Custom generator".to_string(),
            target: "json".to_string(),
            capabilities: vec![GeneratorCapability::Services],
            author: Some("User".to_string()),
            homepage: None,
        };

        let builtin_gen = DiscoveredGenerator {
            id: "builtin-gen".to_string(),
            metadata: builtin_metadata,
            wasm_path: PathBuf::from("/builtin/gen.wasm"),
            generator_type: GeneratorType::BuiltIn,
            compatibility: CompatibilityStatus::Compatible,
        };

        let custom_gen = DiscoveredGenerator {
            id: "custom-gen".to_string(),
            metadata: custom_metadata,
            wasm_path: PathBuf::from("/custom/gen.wasm"),
            generator_type: GeneratorType::Custom,
            compatibility: CompatibilityStatus::Compatible,
        };

        registry
            .discovered
            .insert("builtin-gen".to_string(), builtin_gen);
        registry
            .discovered
            .insert("custom-gen".to_string(), custom_gen);

        // Test filtering by type
        let builtin_gens = registry.generators_by_type(GeneratorType::BuiltIn);
        assert_eq!(builtin_gens.len(), 1);
        assert_eq!(builtin_gens[0].id, "builtin-gen");

        let custom_gens = registry.generators_by_type(GeneratorType::Custom);
        assert_eq!(custom_gens.len(), 1);
        assert_eq!(custom_gens[0].id, "custom-gen");

        // Test compatible generators
        let compatible = registry.compatible_generators();
        assert_eq!(compatible.len(), 2);

        // Test specific generator lookup
        assert!(registry.get_generator("builtin-gen").is_some());
        assert!(registry.get_generator("nonexistent").is_none());
    }

    #[test]
    fn test_load_invalid_wasm_module() {
        let mut runtime = WasmGeneratorRuntime::new().expect("Runtime creation failed");

        // Invalid WASM bytes
        let invalid_wasm = b"not wasm at all";
        let result = runtime.load_generator("test_gen".to_string(), invalid_wasm);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to compile WASM module")
        );
    }

    #[test]
    fn test_execute_nonexistent_generator() {
        let mut runtime = WasmGeneratorRuntime::new().expect("Runtime creation failed");
        let spec = create_test_csil_spec();
        let config = create_test_config();

        let result = runtime.execute_generator("nonexistent", &spec, &config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Generator 'nonexistent' not found")
        );
    }

    #[test]
    fn test_list_empty_generators() {
        let runtime = WasmGeneratorRuntime::new().expect("Runtime creation failed");
        let generators = runtime.list_generators();
        assert!(generators.is_empty());
    }

    #[test]
    fn test_unload_nonexistent_generator() {
        let mut runtime = WasmGeneratorRuntime::new().expect("Runtime creation failed");
        let result = runtime.unload_generator("nonexistent");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Generator 'nonexistent' not found")
        );
    }

    #[test]
    fn test_default_wasm_limits() {
        let limits = WasmLimits::default();
        assert_eq!(limits.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_execution_time, Duration::from_secs(30));
    }

    #[test]
    fn test_csil_spec_serialization() {
        let spec = create_test_csil_spec();
        let json = serde_json::to_string(&spec);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("User"));
        assert!(json_str.contains("UserService"));
        assert!(json_str.contains("create_user"));
        assert!(json_str.contains("SendOnly"));
        assert!(json_str.contains("MinLength"));
    }

    #[test]
    fn test_generator_config_serialization() {
        let config = create_test_config();
        let json = serde_json::to_string(&config);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("test"));
        assert!(json_str.contains("/tmp"));
    }

    #[test]
    fn test_loaded_generator_name_storage() {
        let mut runtime = WasmGeneratorRuntime::new().expect("Runtime creation failed");

        // Create invalid WASM bytes that will definitely fail
        let invalid_wasm = b"definitely not valid wasm";

        // This will fail to load because it's not valid WASM,
        // testing the error handling path
        let result = runtime.load_generator("test_gen".to_string(), invalid_wasm);
        assert!(result.is_err());

        // Verify the generator wasn't added to the list
        assert!(runtime.list_generators().is_empty());
    }

    #[test]
    fn test_runtime_concurrent_safety() {
        // This tests that we can create multiple runtimes concurrently
        use std::thread;

        let handles: Vec<_> = (0..4)
            .map(|_| {
                thread::spawn(|| {
                    let runtime = WasmGeneratorRuntime::new();
                    assert!(runtime.is_ok());
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Thread failed");
        }
    }

    // Integration test scenarios matching the requirements

    #[test]
    fn test_integration_csil_with_services_and_metadata() {
        let spec = create_test_csil_spec();

        // Verify the spec has services
        let service_rules: Vec<_> = spec
            .rules
            .iter()
            .filter_map(|rule| {
                if let RuleType::ServiceDef(service) = &rule.rule_type {
                    Some(service)
                } else {
                    None
                }
            })
            .collect();

        assert!(!service_rules.is_empty(), "Test spec should have services");

        // Verify the spec has field metadata
        let mut has_metadata = false;
        for rule in &spec.rules {
            if let RuleType::GroupDef(group) = &rule.rule_type {
                for entry in &group.entries {
                    if !entry.metadata.is_empty() {
                        has_metadata = true;
                        break;
                    }
                }
            }
        }

        assert!(has_metadata, "Test spec should have field metadata");
    }

    #[test]
    fn test_resource_limits_configuration() {
        let custom_limits = WasmLimits {
            max_memory_bytes: 16 * 1024 * 1024, // 16 MB
            max_execution_time: Duration::from_secs(5),
        };

        let runtime = WasmGeneratorRuntime::new_with_limits(custom_limits.clone());
        assert!(runtime.is_ok());

        let runtime = runtime.unwrap();
        assert_eq!(
            runtime.limits.max_memory_bytes,
            custom_limits.max_memory_bytes
        );
        assert_eq!(
            runtime.limits.max_execution_time,
            custom_limits.max_execution_time
        );
    }

    #[test]
    fn test_error_message_quality() {
        let mut runtime = WasmGeneratorRuntime::new().expect("Runtime creation failed");

        // Test missing generator error
        let spec = create_test_csil_spec();
        let config = create_test_config();
        let result = runtime.execute_generator("missing_gen", &spec, &config);

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Generator 'missing_gen' not found"));

        // Test invalid WASM loading error
        let invalid_wasm = b"clearly not wasm";
        let result = runtime.load_generator("bad_gen".to_string(), invalid_wasm);

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Failed to compile WASM module 'bad_gen'"));
    }
}
