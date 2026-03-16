//! Common error types for csilgen

use thiserror::Error;

/// Location information for errors
#[derive(Debug, Clone)]
pub struct ErrorLocation {
    pub line: usize,
    pub column: usize,
    pub file: Option<String>,
}

impl std::fmt::Display for ErrorLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{}:{}:{}", file, self.line, self.column)
        } else {
            write!(f, "{}:{}", self.line, self.column)
        }
    }
}

/// Structured parse error with location and context
#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub location: Option<ErrorLocation>,
    pub suggestion: Option<String>,
    pub snippet: Option<String>,
    pub error_kind: ParseErrorKind,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(location) = &self.location {
            write!(f, "{}: {}", location, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for ParseError {}

/// Categories of parse errors for better user experience
#[derive(Debug, Clone)]
pub enum ParseErrorKind {
    CddlSyntax,
    ServiceDefinition,
    FieldMetadata,
    UnexpectedToken,
    MissingToken,
    InvalidType,
    CircularReference,
    UnsupportedFeature,
}

/// Structured validation error with context
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub message: String,
    pub location: Option<ErrorLocation>,
    pub suggestion: Option<String>,
    pub error_kind: ValidationErrorKind,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(location) = &self.location {
            write!(f, "{}: {}", location, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for ValidationError {}

/// Categories of validation errors
#[derive(Debug, Clone)]
pub enum ValidationErrorKind {
    UndefinedType,
    ConflictingMetadata,
    InvalidServiceOperation,
    InvalidDependency,
    TypeMismatch,
    MissingRequiredField,
    DuplicateRuleName,
    DuplicateServiceOperationName,
}

/// Common errors that can occur in csilgen operations
#[derive(Error, Debug, Clone)]
pub enum CsilgenError {
    #[error("Parse error: {0}")]
    ParseError(ParseError),

    #[error("Validation error: {0}")]
    ValidationError(ValidationError),

    #[error("Generation error: {0}")]
    GenerationError(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("WASM runtime error: {0}")]
    WasmError(String),

    #[error("Multiple errors occurred")]
    MultipleErrors(Vec<CsilgenError>),

    #[error("Generic error: {0}")]
    GenericError(String),
}

impl CsilgenError {
    /// Create a parse error with helpful suggestions based on common mistakes
    pub fn parse_error_with_suggestion(
        message: &str,
        location: Option<ErrorLocation>,
        kind: ParseErrorKind,
    ) -> Self {
        let suggestion = Self::suggest_fix_for_parse_error(&kind, message);
        CsilgenError::ParseError(ParseError {
            message: message.to_string(),
            location,
            suggestion,
            snippet: None,
            error_kind: kind,
        })
    }

    /// Create a parse error with code snippet for better context
    pub fn parse_error_with_snippet(
        message: &str,
        location: Option<ErrorLocation>,
        kind: ParseErrorKind,
        source_code: &str,
    ) -> Self {
        let suggestion = Self::suggest_fix_for_parse_error(&kind, message);
        let snippet = location
            .as_ref()
            .and_then(|loc| Self::extract_code_snippet(source_code, loc.line, loc.column));

        CsilgenError::ParseError(ParseError {
            message: message.to_string(),
            location,
            suggestion,
            snippet,
            error_kind: kind,
        })
    }

    /// Extract a code snippet around the error location
    fn extract_code_snippet(
        source_code: &str,
        error_line: usize,
        error_column: usize,
    ) -> Option<String> {
        let lines: Vec<&str> = source_code.lines().collect();
        if error_line == 0 || error_line > lines.len() {
            return None;
        }

        let start_line = error_line.saturating_sub(2).max(1);
        let end_line = (error_line + 2).min(lines.len());

        let mut snippet = String::new();
        for (i, line_content) in lines.iter().enumerate().take(end_line).skip(start_line - 1) {
            let line_num = i + 1;
            snippet.push_str(&format!("{line_num:4} | {line_content}\n"));

            // Add error indicator on the error line
            if line_num == error_line {
                let spaces = " ".repeat(6 + error_column.saturating_sub(1));
                snippet.push_str(&format!("{spaces}^ here\n"));
            }
        }

        Some(snippet.trim_end().to_string())
    }

    /// Suggest similar names for typos using edit distance
    pub fn suggest_similar_names(input: &str, available_names: &[String]) -> Vec<String> {
        let mut suggestions: Vec<(String, usize)> = available_names
            .iter()
            .filter_map(|name| {
                let distance = Self::levenshtein_distance(input, name);
                // Only suggest names with edit distance <= 2 and similar length
                if distance <= 2 && name.len().abs_diff(input.len()) <= 2 {
                    Some((name.clone(), distance))
                } else {
                    None
                }
            })
            .collect();

        // Sort by edit distance, then alphabetically
        suggestions.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

        // Return top 3 suggestions
        suggestions
            .into_iter()
            .take(3)
            .map(|(name, _)| name)
            .collect()
    }

    /// Calculate Levenshtein distance between two strings
    fn levenshtein_distance(s1: &str, s2: &str) -> usize {
        let len1 = s1.chars().count();
        let len2 = s2.chars().count();

        if len1 == 0 {
            return len2;
        }
        if len2 == 0 {
            return len1;
        }

        let mut matrix = vec![vec![0; len2 + 1]; len1 + 1];

        // Initialize first row and column
        for (i, row) in matrix.iter_mut().enumerate().take(len1 + 1) {
            row[0] = i;
        }
        for (j, val) in matrix[0].iter_mut().enumerate().take(len2 + 1) {
            *val = j;
        }

        let s1_chars: Vec<char> = s1.chars().collect();
        let s2_chars: Vec<char> = s2.chars().collect();

        // Fill the matrix
        for i in 1..=len1 {
            for j in 1..=len2 {
                let cost = if s1_chars[i - 1] == s2_chars[j - 1] {
                    0
                } else {
                    1
                };
                matrix[i][j] = std::cmp::min(
                    std::cmp::min(
                        matrix[i - 1][j] + 1, // deletion
                        matrix[i][j - 1] + 1, // insertion
                    ),
                    matrix[i - 1][j - 1] + cost, // substitution
                );
            }
        }

        matrix[len1][len2]
    }

    /// Create a validation error with context
    pub fn validation_error_with_context(
        message: &str,
        location: Option<ErrorLocation>,
        kind: ValidationErrorKind,
    ) -> Self {
        let suggestion = Self::suggest_fix_for_validation_error(&kind, message);
        CsilgenError::ValidationError(ValidationError {
            message: message.to_string(),
            location,
            suggestion,
            error_kind: kind,
        })
    }

    /// Create an enhanced WASM error with context and suggestions
    pub fn wasm_error_with_context(message: &str, generator_name: &str) -> Self {
        let enhanced_message = if message.to_lowercase().contains("function") {
            format!(
                "{message}. The generator '{generator_name}' does not implement the required interface. This may be an incompatible or corrupted generator"
            )
        } else if message.contains("not found") {
            format!(
                "{message}. Available generators can be listed with 'csilgen list-generators'. Check if the generator is installed correctly"
            )
        } else if message.contains("compile") {
            format!(
                "{message}. This may indicate a corrupted or incompatible WASM module. Try reinstalling the generator"
            )
        } else if message.to_lowercase().contains("memory") {
            format!(
                "{message}. The generator '{generator_name}' exceeded memory limits. Try processing smaller files or increase WASM memory limits"
            )
        } else if message.to_lowercase().contains("timeout")
            || message.to_lowercase().contains("fuel")
        {
            format!(
                "{message}. The generator '{generator_name}' took too long to execute. Try processing smaller files or increase execution time limits"
            )
        } else {
            format!("Generator '{generator_name}' error: {message}")
        };

        CsilgenError::WasmError(enhanced_message)
    }

    /// Provide additional suggestions for common CSIL mistakes
    pub fn suggest_common_fixes(error_message: &str) -> Option<String> {
        if error_message.contains("Expected identifier") && error_message.contains("service") {
            Some("Service names must be valid identifiers (letters, numbers, underscore). Example: service UserAPI or service User_Service".to_string())
        } else if error_message.contains("Expected") && error_message.contains("->") {
            Some("Service operations need both input and output types separated by '->'. Example: operation: InputType -> OutputType".to_string())
        } else if error_message.contains("metadata") && error_message.contains("@") {
            Some("Field metadata annotations start with '@' and go before the field. Example: @send-only\\nfield_name: type".to_string())
        } else if error_message.contains("circular") {
            Some("To break circular dependencies, consider making one field optional (?) or restructuring your types".to_string())
        } else if error_message.contains("undefined") || error_message.contains("not found") {
            Some("Check spelling and ensure all types are defined before use. Use 'include \"file.csil\"' for external types".to_string())
        } else {
            None
        }
    }

    /// Suggest fixes for common parse errors with enhanced context
    fn suggest_fix_for_parse_error(kind: &ParseErrorKind, message: &str) -> Option<String> {
        match kind {
            ParseErrorKind::ServiceDefinition => {
                if message.contains("<->") || message.to_lowercase().contains("bidirectional") {
                    Some("Bidirectional service operations use: operation-name: InputType <-> OutputType for real-time services".to_string())
                } else if message.contains("<-") || message.to_lowercase().contains("reverse") {
                    Some("Reverse service operations use: operation-name: InputType <- OutputType for push-style operations".to_string())
                } else if message.contains("->") {
                    Some("Service operations should use the format: operation-name: InputType -> OutputType. Example: create-user: CreateUserRequest -> UserProfile".to_string())
                } else {
                    Some("Service definitions should use the format: service ServiceName { operation: Type -> Type }. Example: service UserAPI { get-user: {id: int} -> UserProfile }".to_string())
                }
            }
            ParseErrorKind::FieldMetadata => {
                if message.contains("@depends-on") {
                    Some("@depends-on syntax: @depends-on(field_name) or @depends-on(field_name = value). Example: @depends-on(type = \"premium\")".to_string())
                } else if message.contains("@min-length") || message.contains("@max-length") {
                    Some("Length constraints require a positive integer: @min-length(8) or @max-length(255)".to_string())
                } else if message.contains("@min-items") || message.contains("@max-items") {
                    Some("Item constraints require a non-negative integer: @min-items(1) or @max-items(100)".to_string())
                } else {
                    Some("Field metadata should use format: @annotation or @annotation(params). Common annotations: @send-only, @receive-only, @depends-on(field = value), @description(\"text\")".to_string())
                }
            }
            ParseErrorKind::MissingToken => {
                if message.contains("'{'") {
                    Some("Missing opening brace '{' - check if you forgot to start a group or service definition. Groups: { field: type }, Services: service Name { ... }".to_string())
                } else if message.contains("'}'") {
                    Some("Missing closing brace '}' - check if you have unmatched braces. Each '{' needs a matching '}'".to_string())
                } else if message.contains("':'") {
                    Some("Missing colon ':' - in groups, fields need format 'name: type'. In services, operations need 'name: input -> output'".to_string())
                } else if message.contains("','") {
                    Some("Missing comma ',' - separate multiple fields/operations with commas. Last item can omit the comma".to_string())
                } else {
                    Some("Check for missing punctuation like commas, colons, or braces. CSIL is whitespace-tolerant but punctuation is required".to_string())
                }
            }
            ParseErrorKind::UnexpectedToken => {
                if message.contains("service") {
                    Some("Did you mean to define a service? Format: service ServiceName { operation: Type -> Type }".to_string())
                } else if message.contains("@") {
                    Some("Did you mean to add field metadata? Common annotations: @send-only, @receive-only, @depends-on(field), @description(\"text\")".to_string())
                } else {
                    Some("Check for typos in keywords or incorrect syntax. Common CSIL keywords: service, include, from, options, @send-only, @receive-only, @depends-on".to_string())
                }
            }
            ParseErrorKind::InvalidType => {
                Some("Invalid type reference. Built-in types: int, text, bool, bytes, float. Custom types must be defined elsewhere in the file or imported".to_string())
            }
            ParseErrorKind::CircularReference => {
                Some("Circular type reference detected. Type definitions cannot directly or indirectly reference themselves. Consider using optional fields or breaking the cycle".to_string())
            }
            ParseErrorKind::UnsupportedFeature => {
                if message.contains(".ne") {
                    Some("The .ne (not equal) constraint is planned for a future release. Consider using .eq with logical restructuring or supported constraints".to_string())
                } else if message.contains(".bits") {
                    Some("The .bits (bit control) constraint is planned for a future release. Use integer types with range constraints for bitwise operations".to_string())
                } else if message.contains(".and") {
                    Some("The .and (type intersection) constraint is planned for a future release. Use nested group definitions for complex type combinations".to_string())
                } else if message.contains(".within") {
                    Some("The .within (subset constraint) is planned for a future release. Use explicit enumeration or range constraints instead".to_string())
                } else if message.contains(".json") {
                    Some("The .json (JSON encoding) constraint is planned for a future release. Use text types with .regex validation for JSON strings".to_string())
                } else if message.contains(".cbor") {
                    Some("The .cbor (CBOR encoding) constraint is planned for a future release. Use bytes type for CBOR data".to_string())
                } else if message.contains(".cborseq") {
                    Some("The .cborseq (CBOR sequence) constraint is planned for a future release. Use arrays of bytes for CBOR sequences".to_string())
                } else {
                    Some("This CDDL feature is not yet supported but is planned for a future release. Check the documentation for supported syntax alternatives".to_string())
                }
            }
            _ => None,
        }
    }

    /// Suggest fixes for common validation errors with enhanced context
    fn suggest_fix_for_validation_error(
        kind: &ValidationErrorKind,
        message: &str,
    ) -> Option<String> {
        match kind {
            ValidationErrorKind::UndefinedType => {
                if message.to_lowercase().contains("service") {
                    Some("Service input/output types must be defined. Either define the type in the same file, import it, or use an inline group: { field: type }".to_string())
                } else {
                    Some("Make sure the type is defined elsewhere in the CSIL file or is a built-in CDDL type (int, text, bool, bytes, float). Use 'include \"file.csil\"' for external types".to_string())
                }
            }
            ValidationErrorKind::ConflictingMetadata => {
                if message.contains("visibility") {
                    Some("A field can only have one visibility setting. Choose either @send-only, @receive-only, or @bidirectional".to_string())
                } else if message.contains("constraint") {
                    Some("Duplicate constraint detected. Each constraint type (@min-length, @max-length, etc.) can only appear once per field".to_string())
                } else {
                    Some("Check that field metadata doesn't conflict (e.g., both @send-only and @receive-only on the same field)".to_string())
                }
            }
            ValidationErrorKind::InvalidServiceOperation => {
                if message.contains("direction") {
                    Some("Service operations must specify direction: use -> for unidirectional, <- for reverse, or <-> for bidirectional communication".to_string())
                } else {
                    Some("Service operations must specify both input and output types using a direction operator. Format: operation-name: InputType -> OutputType".to_string())
                }
            }
            ValidationErrorKind::InvalidDependency => {
                if message.to_lowercase().contains("circular") {
                    Some("Circular dependencies detected in @depends-on annotations. Field A cannot depend on field B if B depends on A (directly or indirectly)".to_string())
                } else {
                    Some("Dependencies in @depends-on must reference existing fields in the same type definition. Check field names for typos".to_string())
                }
            }
            ValidationErrorKind::TypeMismatch => {
                Some("Type mismatch detected. Check that assigned values match the expected type (e.g., text values for text fields, integers for int fields)".to_string())
            }
            ValidationErrorKind::MissingRequiredField => {
                Some("Required field is missing. Fields without '?' are required and must be present in all instances of this type".to_string())
            }
            ValidationErrorKind::DuplicateRuleName => {
                Some("Rule names must be unique within a CSIL specification. Consider using different names or organizing rules into separate files with imports".to_string())
            }
            ValidationErrorKind::DuplicateServiceOperationName => {
                Some("Operation names must be unique within each service. Use different operation names or split operations into separate services".to_string())
            }
        }
    }

    /// Check if this error should cause the program to exit with failure
    pub fn is_fatal(&self) -> bool {
        match self {
            CsilgenError::ParseError(_) => true,
            CsilgenError::ValidationError(_) => true,
            CsilgenError::IoError(_) => true,
            CsilgenError::MultipleErrors(errors) => errors.iter().any(|e| e.is_fatal()),
            _ => false,
        }
    }

    /// Get all error messages in a flattened list
    pub fn get_all_messages(&self) -> Vec<String> {
        match self {
            CsilgenError::MultipleErrors(errors) => {
                errors.iter().flat_map(|e| e.get_all_messages()).collect()
            }
            _ => vec![self.to_string()],
        }
    }

    /// Get error priority for sorting (lower number = higher priority)
    pub fn priority(&self) -> u8 {
        match self {
            CsilgenError::ParseError(parse_err) => {
                match parse_err.error_kind {
                    ParseErrorKind::CddlSyntax => 1, // Critical syntax errors first
                    ParseErrorKind::ServiceDefinition => 2, // Service errors are important
                    ParseErrorKind::FieldMetadata => 3, // Metadata errors after structure
                    ParseErrorKind::MissingToken | ParseErrorKind::UnexpectedToken => 4,
                    ParseErrorKind::InvalidType => 5,
                    ParseErrorKind::CircularReference => 6,
                    ParseErrorKind::UnsupportedFeature => 7, // Unsupported features are informational
                }
            }
            CsilgenError::ValidationError(val_err) => {
                match val_err.error_kind {
                    ValidationErrorKind::UndefinedType => 2, // Type errors are critical
                    ValidationErrorKind::DuplicateRuleName => 2, // Duplicate names are critical
                    ValidationErrorKind::InvalidServiceOperation => 3,
                    ValidationErrorKind::DuplicateServiceOperationName => 3, // Service naming issues
                    ValidationErrorKind::ConflictingMetadata => 4,
                    ValidationErrorKind::InvalidDependency => 5,
                    ValidationErrorKind::TypeMismatch => 6,
                    ValidationErrorKind::MissingRequiredField => 7,
                }
            }
            CsilgenError::IoError(_) => 1, // IO errors are critical
            CsilgenError::ConfigError(_) => 8, // Config errors are less critical
            CsilgenError::WasmError(_) => 9, // WASM errors after parsing/validation
            CsilgenError::GenerationError(_) => 10, // Generation errors last
            CsilgenError::MultipleErrors(_) => 255, // Should not be prioritized directly
            CsilgenError::GenericError(_) => 128, // Generic errors in middle
        }
    }

    /// Sort errors by priority and group similar errors
    pub fn sort_errors_by_priority(errors: Vec<CsilgenError>) -> Vec<CsilgenError> {
        let mut sorted_errors = errors;
        sorted_errors.sort_by(|a, b| {
            a.priority()
                .cmp(&b.priority())
                .then_with(|| a.to_string().cmp(&b.to_string()))
        });
        sorted_errors
    }
}

impl From<ParseError> for CsilgenError {
    fn from(err: ParseError) -> Self {
        CsilgenError::ParseError(err)
    }
}

impl From<ValidationError> for CsilgenError {
    fn from(err: ValidationError) -> Self {
        CsilgenError::ValidationError(err)
    }
}

impl From<std::io::Error> for CsilgenError {
    fn from(err: std::io::Error) -> Self {
        CsilgenError::IoError(err.to_string())
    }
}

impl From<anyhow::Error> for CsilgenError {
    fn from(err: anyhow::Error) -> Self {
        CsilgenError::GenericError(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, CsilgenError>;
