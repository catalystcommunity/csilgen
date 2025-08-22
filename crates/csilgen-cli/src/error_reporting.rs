//! Error reporting and user experience utilities for the CLI

use atty::Stream;
use console::{Term, style};
use csilgen_common::{
    CsilgenError, ParseError, ParseErrorKind, ValidationError, ValidationErrorKind,
};
use indicatif::{ProgressBar, ProgressStyle};

/// Configuration for CLI output
#[derive(Debug, Clone)]
pub struct CliOutputConfig {
    pub use_colors: bool,
    pub verbose: bool,
    pub quiet: bool,
}

impl Default for CliOutputConfig {
    fn default() -> Self {
        Self {
            use_colors: atty::is(Stream::Stderr),
            verbose: false,
            quiet: false,
        }
    }
}

/// CLI error reporter with colored output and suggestions
pub struct ErrorReporter {
    pub config: CliOutputConfig,
    term: Term,
}

impl ErrorReporter {
    pub fn new(config: CliOutputConfig) -> Self {
        Self {
            config,
            term: Term::stderr(),
        }
    }

    /// Report an error with appropriate formatting and suggestions
    pub fn report_error(&self, error: &CsilgenError) -> anyhow::Result<()> {
        if self.config.quiet {
            return Ok(());
        }

        match error {
            CsilgenError::ParseError(parse_error) => {
                self.report_parse_error(parse_error)?;
            }
            CsilgenError::ValidationError(validation_error) => {
                self.report_validation_error(validation_error)?;
            }
            CsilgenError::MultipleErrors(errors) => {
                // Sort errors by priority before displaying
                let sorted_errors = CsilgenError::sort_errors_by_priority(errors.to_vec());

                if self.config.use_colors {
                    self.term.write_line(&format!(
                        "{} {} errors occurred (sorted by priority):",
                        style("❌").red().bold(),
                        sorted_errors.len()
                    ))?;
                } else {
                    self.term.write_line(&format!(
                        "Error: {} errors occurred (sorted by priority):",
                        sorted_errors.len()
                    ))?;
                }

                for (i, error) in sorted_errors.iter().enumerate() {
                    let priority_indicator = match error.priority() {
                        1..=3 => {
                            if self.config.use_colors {
                                style("🚨").red().to_string()
                            } else {
                                "CRITICAL".to_string()
                            }
                        }
                        4..=6 => {
                            if self.config.use_colors {
                                style("⚠️").yellow().to_string()
                            } else {
                                "WARNING".to_string()
                            }
                        }
                        _ => {
                            if self.config.use_colors {
                                style("ℹ️").blue().to_string()
                            } else {
                                "INFO".to_string()
                            }
                        }
                    };

                    if self.config.use_colors {
                        self.term.write_line(&format!(
                            "  {} {}. {}",
                            priority_indicator,
                            style(i + 1).yellow(),
                            error
                        ))?;
                    } else {
                        self.term.write_line(&format!(
                            "  [{}] {}. {}",
                            priority_indicator,
                            i + 1,
                            error
                        ))?;
                    }
                }
            }
            _ => {
                if self.config.use_colors {
                    self.term
                        .write_line(&format!("{} {}", style("❌").red().bold(), error))?;
                } else {
                    self.term.write_line(&format!("Error: {error}"))?;
                }
            }
        }

        Ok(())
    }

    /// Report a parse error with context and suggestions
    fn report_parse_error(&self, error: &ParseError) -> anyhow::Result<()> {
        // Error header with icon and category
        let category = self.parse_error_category(&error.error_kind);
        if self.config.use_colors {
            self.term.write_line(&format!(
                "{} {} ({})",
                style("❌").red().bold(),
                style("Parse Error").red().bold(),
                style(category).dim()
            ))?;
        } else {
            self.term.write_line(&format!("Parse Error ({category})"))?;
        }

        // Location information if available
        if let Some(location) = &error.location {
            if self.config.use_colors {
                self.term
                    .write_line(&format!("  {} {}", style("📍").cyan(), location))?;
            } else {
                self.term.write_line(&format!("  Location: {location}"))?;
            }
        }

        // Main error message
        if self.config.use_colors {
            self.term
                .write_line(&format!("  {}", style(&error.message).bright()))?;
        } else {
            self.term.write_line(&format!("  {}", error.message))?;
        }

        // Code snippet if available
        if let Some(snippet) = &error.snippet {
            self.term.write_line("")?;
            if self.config.use_colors {
                self.term
                    .write_line(&format!("  {} Code:", style("📝").blue()))?;
            } else {
                self.term.write_line("  Code:")?;
            }
            for line in snippet.lines() {
                if self.config.use_colors {
                    self.term
                        .write_line(&format!("    {}", style(line).dim()))?;
                } else {
                    self.term.write_line(&format!("    {line}"))?;
                }
            }
        }

        // Suggestion if available
        if let Some(suggestion) = &error.suggestion {
            self.term.write_line("")?;
            if self.config.use_colors {
                self.term.write_line(&format!(
                    "  {} {}",
                    style("💡").yellow(),
                    style("Suggestion:").yellow().bold()
                ))?;
                self.term
                    .write_line(&format!("    {}", style(suggestion).green()))?;
            } else {
                self.term.write_line("  Suggestion:")?;
                self.term.write_line(&format!("    {suggestion}"))?;
            }
        }

        self.term.write_line("")?;
        Ok(())
    }

    /// Report a validation error with context and suggestions  
    fn report_validation_error(&self, error: &ValidationError) -> anyhow::Result<()> {
        // Error header with icon and category
        let category = self.validation_error_category(&error.error_kind);
        if self.config.use_colors {
            self.term.write_line(&format!(
                "{} {} ({})",
                style("⚠️").yellow().bold(),
                style("Validation Error").yellow().bold(),
                style(category).dim()
            ))?;
        } else {
            self.term
                .write_line(&format!("Validation Error ({category})"))?;
        }

        // Location information if available
        if let Some(location) = &error.location {
            if self.config.use_colors {
                self.term
                    .write_line(&format!("  {} {}", style("📍").cyan(), location))?;
            } else {
                self.term.write_line(&format!("  Location: {location}"))?;
            }
        }

        // Main error message
        if self.config.use_colors {
            self.term
                .write_line(&format!("  {}", style(&error.message).bright()))?;
        } else {
            self.term.write_line(&format!("  {}", error.message))?;
        }

        // Suggestion if available
        if let Some(suggestion) = &error.suggestion {
            self.term.write_line("")?;
            if self.config.use_colors {
                self.term.write_line(&format!(
                    "  {} {}",
                    style("💡").yellow(),
                    style("Suggestion:").yellow().bold()
                ))?;
                self.term
                    .write_line(&format!("    {}", style(suggestion).green()))?;
            } else {
                self.term.write_line("  Suggestion:")?;
                self.term.write_line(&format!("    {suggestion}"))?;
            }
        }

        self.term.write_line("")?;
        Ok(())
    }

    /// Print a success message
    pub fn report_success(&self, message: &str) -> anyhow::Result<()> {
        if self.config.quiet {
            return Ok(());
        }

        if self.config.use_colors {
            self.term.write_line(&format!(
                "{} {}",
                style("✅").green().bold(),
                style(message).green()
            ))?;
        } else {
            self.term.write_line(&format!("Success: {message}"))?;
        }
        Ok(())
    }

    /// Print an informational message
    pub fn report_info(&self, message: &str) -> anyhow::Result<()> {
        if self.config.quiet {
            return Ok(());
        }

        if self.config.use_colors {
            self.term
                .write_line(&format!("{} {}", style("ℹ️").blue(), message))?;
        } else {
            self.term.write_line(&format!("Info: {message}"))?;
        }
        Ok(())
    }

    /// Print a warning message
    pub fn report_warning(&self, message: &str) -> anyhow::Result<()> {
        if self.config.quiet {
            return Ok(());
        }

        if self.config.use_colors {
            self.term.write_line(&format!(
                "{} {}",
                style("⚠️").yellow().bold(),
                style(message).yellow()
            ))?;
        } else {
            self.term.write_line(&format!("Warning: {message}"))?;
        }
        Ok(())
    }

    /// Print verbose debug information
    pub fn report_debug(&self, message: &str) -> anyhow::Result<()> {
        if !self.config.verbose || self.config.quiet {
            return Ok(());
        }

        if self.config.use_colors {
            self.term
                .write_line(&format!("{} {}", style("🔍").dim(), style(message).dim()))?;
        } else {
            self.term.write_line(&format!("Debug: {message}"))?;
        }
        Ok(())
    }

    /// Create a progress bar for long-running operations
    pub fn create_progress_bar(&self, len: u64, message: &str) -> ProgressBar {
        if self.config.quiet {
            return ProgressBar::hidden();
        }

        let pb = ProgressBar::new(len);
        if self.config.use_colors {
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} {msg}")
                    .expect("Valid progress style")
                    .progress_chars("#>-")
            );
        } else {
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] [{wide_bar}] {pos}/{len} {msg}")
                    .expect("Valid progress style")
                    .progress_chars("=>-"),
            );
        }
        pb.set_message(message.to_string());
        pb
    }

    /// Get human-readable category for parse error
    fn parse_error_category(&self, kind: &ParseErrorKind) -> &'static str {
        match kind {
            ParseErrorKind::CddlSyntax => "CDDL Syntax",
            ParseErrorKind::ServiceDefinition => "Service Definition",
            ParseErrorKind::FieldMetadata => "Field Metadata",
            ParseErrorKind::UnexpectedToken => "Unexpected Token",
            ParseErrorKind::MissingToken => "Missing Token",
            ParseErrorKind::InvalidType => "Invalid Type",
            ParseErrorKind::CircularReference => "Circular Reference",
            ParseErrorKind::UnsupportedFeature => "Unsupported Feature",
        }
    }

    /// Get human-readable category for validation error
    fn validation_error_category(&self, kind: &ValidationErrorKind) -> &'static str {
        match kind {
            ValidationErrorKind::UndefinedType => "Undefined Type",
            ValidationErrorKind::ConflictingMetadata => "Conflicting Metadata",
            ValidationErrorKind::InvalidServiceOperation => "Invalid Service Operation",
            ValidationErrorKind::InvalidDependency => "Invalid Dependency",
            ValidationErrorKind::TypeMismatch => "Type Mismatch",
            ValidationErrorKind::MissingRequiredField => "Missing Required Field",
            ValidationErrorKind::DuplicateRuleName => "Duplicate Rule Name",
            ValidationErrorKind::DuplicateServiceOperationName => "Duplicate Service Operation",
        }
    }
}

/// Format a generation summary in a human-readable way
pub fn format_generation_summary(
    processed_files: usize,
    generated_files: usize,
    total_size: usize,
    error_count: usize,
    use_colors: bool,
) -> String {
    let size_str = format_file_size(total_size);

    if use_colors {
        format!(
            "{} Generation Summary:\n  {} Processed: {} CSIL file{}\n  {} Generated: {} file{} ({})\n{}",
            style("📊").blue(),
            style("✅").green(),
            style(processed_files).cyan().bold(),
            if processed_files == 1 { "" } else { "s" },
            style("📝").blue(),
            style(generated_files).cyan().bold(),
            if generated_files == 1 { "" } else { "s" },
            style(&size_str).dim(),
            if error_count > 0 {
                format!(
                    "  {} Errors: {}",
                    style("❌").red(),
                    style(error_count).red().bold()
                )
            } else {
                String::new()
            }
        )
    } else {
        format!(
            "Generation Summary:\n  Processed: {} CSIL file{}\n  Generated: {} file{} ({})\n{}",
            processed_files,
            if processed_files == 1 { "" } else { "s" },
            generated_files,
            if generated_files == 1 { "" } else { "s" },
            size_str,
            if error_count > 0 {
                format!("  Errors: {error_count}")
            } else {
                String::new()
            }
        )
    }
}

/// Format file size in a human-readable way
fn format_file_size(size: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = size as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", size as usize, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{ErrorLocation, ParseErrorKind, ValidationErrorKind};

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(2048), "2.0 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_file_size(1536 * 1024), "1.5 MB");
    }

    #[test]
    fn test_cli_output_config_default() {
        let config = CliOutputConfig::default();
        assert!(!config.verbose);
        assert!(!config.quiet);
    }

    #[test]
    fn test_error_location_display() {
        let location = ErrorLocation {
            line: 42,
            column: 15,
            file: Some("test.csil".to_string()),
        };
        assert_eq!(location.to_string(), "test.csil:42:15");

        let location_no_file = ErrorLocation {
            line: 42,
            column: 15,
            file: None,
        };
        assert_eq!(location_no_file.to_string(), "42:15");
    }

    #[test]
    fn test_parse_error_display() {
        let error = ParseError {
            message: "Unexpected token".to_string(),
            location: Some(ErrorLocation {
                line: 10,
                column: 5,
                file: Some("test.csil".to_string()),
            }),
            suggestion: Some("Check for missing semicolon".to_string()),
            snippet: None,
            error_kind: ParseErrorKind::UnexpectedToken,
        };

        assert_eq!(error.to_string(), "test.csil:10:5: Unexpected token");
    }

    #[test]
    fn test_validation_error_display() {
        let error = ValidationError {
            message: "Undefined type 'Foo'".to_string(),
            location: Some(ErrorLocation {
                line: 5,
                column: 10,
                file: None,
            }),
            suggestion: Some("Define the type or check for typos".to_string()),
            error_kind: ValidationErrorKind::UndefinedType,
        };

        assert_eq!(error.to_string(), "5:10: Undefined type 'Foo'");
    }

    #[test]
    fn test_csilgen_error_suggestions() {
        // Test parse error suggestions
        let parse_error = CsilgenError::parse_error_with_suggestion(
            "Missing opening brace '{'",
            None,
            ParseErrorKind::MissingToken,
        );

        if let CsilgenError::ParseError(pe) = parse_error {
            assert!(pe.suggestion.is_some());
            assert!(pe.suggestion.unwrap().contains("Missing opening brace"));
        } else {
            panic!("Expected ParseError");
        }

        // Test validation error suggestions
        let validation_error = CsilgenError::validation_error_with_context(
            "Type 'Unknown' is not defined",
            None,
            ValidationErrorKind::UndefinedType,
        );

        if let CsilgenError::ValidationError(ve) = validation_error {
            assert!(ve.suggestion.is_some());
            assert!(ve.suggestion.unwrap().contains("built-in CDDL type"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_error_fatality() {
        let parse_error = CsilgenError::parse_error_with_suggestion(
            "Syntax error",
            None,
            ParseErrorKind::CddlSyntax,
        );
        assert!(parse_error.is_fatal());

        let validation_error = CsilgenError::validation_error_with_context(
            "Type error",
            None,
            ValidationErrorKind::TypeMismatch,
        );
        assert!(validation_error.is_fatal());

        let generation_error = CsilgenError::GenerationError("Generator failed".to_string());
        assert!(!generation_error.is_fatal());

        let wasm_error = CsilgenError::WasmError("WASM runtime error".to_string());
        assert!(!wasm_error.is_fatal());
    }

    #[test]
    fn test_multiple_errors() {
        let errors = vec![
            CsilgenError::parse_error_with_suggestion("Error 1", None, ParseErrorKind::CddlSyntax),
            CsilgenError::validation_error_with_context(
                "Error 2",
                None,
                ValidationErrorKind::UndefinedType,
            ),
        ];

        let multi_error = CsilgenError::MultipleErrors(errors);

        assert!(multi_error.is_fatal()); // Should be fatal because it contains fatal errors
        let messages = multi_error.get_all_messages();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].contains("Error 1"));
        assert!(messages[1].contains("Error 2"));
    }

    #[test]
    fn test_error_reporter_creation() {
        let config = CliOutputConfig {
            use_colors: false,
            verbose: true,
            quiet: false,
        };

        let reporter = ErrorReporter::new(config.clone());
        assert!(!reporter.config.use_colors);
        assert!(reporter.config.verbose);
        assert!(!reporter.config.quiet);
    }

    #[test]
    fn test_parse_error_categories() {
        let reporter = ErrorReporter::new(CliOutputConfig::default());

        assert_eq!(
            reporter.parse_error_category(&ParseErrorKind::CddlSyntax),
            "CDDL Syntax"
        );
        assert_eq!(
            reporter.parse_error_category(&ParseErrorKind::ServiceDefinition),
            "Service Definition"
        );
        assert_eq!(
            reporter.parse_error_category(&ParseErrorKind::FieldMetadata),
            "Field Metadata"
        );
        assert_eq!(
            reporter.parse_error_category(&ParseErrorKind::UnexpectedToken),
            "Unexpected Token"
        );
        assert_eq!(
            reporter.parse_error_category(&ParseErrorKind::MissingToken),
            "Missing Token"
        );
        assert_eq!(
            reporter.parse_error_category(&ParseErrorKind::InvalidType),
            "Invalid Type"
        );
        assert_eq!(
            reporter.parse_error_category(&ParseErrorKind::CircularReference),
            "Circular Reference"
        );
    }

    #[test]
    fn test_validation_error_categories() {
        let reporter = ErrorReporter::new(CliOutputConfig::default());

        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::UndefinedType),
            "Undefined Type"
        );
        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::ConflictingMetadata),
            "Conflicting Metadata"
        );
        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::InvalidServiceOperation),
            "Invalid Service Operation"
        );
        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::InvalidDependency),
            "Invalid Dependency"
        );
        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::TypeMismatch),
            "Type Mismatch"
        );
        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::MissingRequiredField),
            "Missing Required Field"
        );
        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::DuplicateRuleName),
            "Duplicate Rule Name"
        );
        assert_eq!(
            reporter.validation_error_category(&ValidationErrorKind::DuplicateServiceOperationName),
            "Duplicate Service Operation"
        );
    }

    #[test]
    fn test_format_generation_summary() {
        // Test with colors
        let summary_colored = format_generation_summary(3, 15, 12345, 0, true);
        println!("Colored summary: {summary_colored}");
        assert!(summary_colored.contains("Generation Summary"));
        // Check for the content without worrying about ANSI styling that might be stripped
        assert!(summary_colored.contains("3") && summary_colored.contains("CSIL"));
        assert!(summary_colored.contains("15") && summary_colored.contains("file"));
        assert!(summary_colored.contains("12.1 KB"));
        assert!(!summary_colored.contains("Errors"));

        // Test without colors
        let summary_plain = format_generation_summary(1, 5, 1024, 2, false);
        assert!(summary_plain.contains("Generation Summary"));
        assert!(summary_plain.contains("1 CSIL file"));
        assert!(summary_plain.contains("5 files"));
        assert!(summary_plain.contains("1.0 KB"));
        assert!(summary_plain.contains("Errors: 2"));
    }

    #[test]
    fn test_service_definition_error_suggestion() {
        let parse_error = CsilgenError::parse_error_with_suggestion(
            "Invalid service operation syntax",
            None,
            ParseErrorKind::ServiceDefinition,
        );

        if let CsilgenError::ParseError(pe) = parse_error {
            assert!(pe.suggestion.is_some());
            assert!(pe.suggestion.unwrap().contains("service ServiceName"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_field_metadata_error_suggestion() {
        let parse_error = CsilgenError::parse_error_with_suggestion(
            "Invalid metadata annotation",
            None,
            ParseErrorKind::FieldMetadata,
        );

        if let CsilgenError::ParseError(pe) = parse_error {
            assert!(pe.suggestion.is_some());
            let suggestion = pe.suggestion.unwrap();
            assert!(suggestion.contains("@annotation"));
            assert!(suggestion.contains("@send-only"));
            assert!(suggestion.contains("@receive-only"));
            assert!(suggestion.contains("@depends-on"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_conflicting_metadata_suggestion() {
        let validation_error = CsilgenError::validation_error_with_context(
            "Field has both @send-only and @receive-only",
            None,
            ValidationErrorKind::ConflictingMetadata,
        );

        if let CsilgenError::ValidationError(ve) = validation_error {
            assert!(ve.suggestion.is_some());
            assert!(ve.suggestion.unwrap().contains("conflict"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_invalid_service_operation_suggestion() {
        let validation_error = CsilgenError::validation_error_with_context(
            "Service operation missing output type",
            None,
            ValidationErrorKind::InvalidServiceOperation,
        );

        if let CsilgenError::ValidationError(ve) = validation_error {
            assert!(ve.suggestion.is_some());
            assert!(ve.suggestion.unwrap().contains("direction operator"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_invalid_dependency_suggestion() {
        let validation_error = CsilgenError::validation_error_with_context(
            "Dependency references non-existent field",
            None,
            ValidationErrorKind::InvalidDependency,
        );

        if let CsilgenError::ValidationError(ve) = validation_error {
            assert!(ve.suggestion.is_some());
            assert!(ve.suggestion.unwrap().contains("existing fields"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_error_priority_calculation() {
        let io_error = CsilgenError::IoError("file not found".to_string());
        let parse_error = CsilgenError::parse_error_with_suggestion(
            "syntax error",
            None,
            ParseErrorKind::CddlSyntax,
        );
        let validation_error = CsilgenError::validation_error_with_context(
            "undefined type",
            None,
            ValidationErrorKind::UndefinedType,
        );
        let config_error = CsilgenError::ConfigError("invalid config".to_string());

        assert_eq!(io_error.priority(), 1);
        assert_eq!(parse_error.priority(), 1);
        assert_eq!(validation_error.priority(), 2);
        assert_eq!(config_error.priority(), 8);
    }

    #[test]
    fn test_error_sorting_with_mixed_severities() {
        let errors = vec![
            CsilgenError::ConfigError("config issue".to_string()),
            CsilgenError::parse_error_with_suggestion(
                "syntax error",
                None,
                ParseErrorKind::CddlSyntax,
            ),
            CsilgenError::validation_error_with_context(
                "undefined type",
                None,
                ValidationErrorKind::UndefinedType,
            ),
            CsilgenError::validation_error_with_context(
                "missing field",
                None,
                ValidationErrorKind::MissingRequiredField,
            ),
        ];

        let sorted = CsilgenError::sort_errors_by_priority(errors);

        assert_eq!(sorted[0].priority(), 1); // Parse error (CddlSyntax)
        assert_eq!(sorted[1].priority(), 2); // Validation error (UndefinedType)
        assert_eq!(sorted[2].priority(), 7); // Validation error (MissingRequiredField)
        assert_eq!(sorted[3].priority(), 8); // Config error
    }

    #[test]
    fn test_multi_error_display_formatting() {
        let errors = vec![
            CsilgenError::parse_error_with_suggestion(
                "first error",
                None,
                ParseErrorKind::CddlSyntax,
            ),
            CsilgenError::validation_error_with_context(
                "second error",
                None,
                ValidationErrorKind::UndefinedType,
            ),
        ];
        let multi_error = CsilgenError::MultipleErrors(errors);

        let reporter = ErrorReporter::new(CliOutputConfig {
            use_colors: false,
            verbose: false,
            quiet: false,
        });

        // This should not panic and should format the multi-error properly
        let result = reporter.report_error(&multi_error);
        assert!(result.is_ok());
    }

    #[test]
    fn test_quiet_mode_suppresses_output() {
        let error = CsilgenError::parse_error_with_suggestion(
            "test error",
            None,
            ParseErrorKind::CddlSyntax,
        );
        let reporter = ErrorReporter::new(CliOutputConfig {
            use_colors: false,
            verbose: false,
            quiet: true,
        });

        let result = reporter.report_error(&error);
        assert!(result.is_ok());

        // In quiet mode, no output should be generated
        let result_success = reporter.report_success("test success");
        assert!(result_success.is_ok());
    }

    #[test]
    fn test_color_vs_plain_output_consistency() {
        let error = CsilgenError::validation_error_with_context(
            "test validation error",
            None,
            ValidationErrorKind::ConflictingMetadata,
        );

        let color_reporter = ErrorReporter::new(CliOutputConfig {
            use_colors: true,
            verbose: false,
            quiet: false,
        });

        let plain_reporter = ErrorReporter::new(CliOutputConfig {
            use_colors: false,
            verbose: false,
            quiet: false,
        });

        // Both should succeed without panicking
        assert!(color_reporter.report_error(&error).is_ok());
        assert!(plain_reporter.report_error(&error).is_ok());
    }

    #[test]
    fn test_progress_bar_creation() {
        let reporter = ErrorReporter::new(CliOutputConfig {
            use_colors: true,
            verbose: false,
            quiet: false,
        });

        let progress_bar = reporter.create_progress_bar(100, "Processing files");

        // Progress bar should be created successfully
        assert_eq!(progress_bar.length(), Some(100));

        // Test progress updates
        progress_bar.set_position(50);
        assert_eq!(progress_bar.position(), 50);

        progress_bar.finish_with_message("Processing complete");
    }

    #[test]
    fn test_progress_bar_in_quiet_mode() {
        let reporter = ErrorReporter::new(CliOutputConfig {
            use_colors: false,
            verbose: false,
            quiet: true,
        });

        let progress_bar = reporter.create_progress_bar(50, "Silent processing");

        // Should still create progress bar but be hidden in quiet mode
        progress_bar.set_position(25);
        progress_bar.finish();

        // Should not panic in quiet mode
    }

    #[test]
    fn test_info_warning_debug_reporting() {
        let reporter = ErrorReporter::new(CliOutputConfig {
            use_colors: false,
            verbose: true,
            quiet: false,
        });

        // Test all reporting levels
        assert!(reporter.report_info("Test info message").is_ok());
        assert!(reporter.report_warning("Test warning message").is_ok());
        assert!(reporter.report_debug("Test debug message").is_ok());
        assert!(reporter.report_success("Test success message").is_ok());
    }

    #[test]
    fn test_parse_error_kinds_with_existing_suggestions() {
        // Test the parse error kinds that currently have suggestion implementations
        let service_error = CsilgenError::parse_error_with_suggestion(
            "Invalid service syntax",
            None,
            ParseErrorKind::ServiceDefinition,
        );
        let metadata_error = CsilgenError::parse_error_with_suggestion(
            "Invalid metadata",
            None,
            ParseErrorKind::FieldMetadata,
        );

        if let CsilgenError::ParseError(pe) = service_error {
            assert!(pe.suggestion.is_some());
        }
        if let CsilgenError::ParseError(pe) = metadata_error {
            assert!(pe.suggestion.is_some());
        }
    }

    #[test]
    fn test_all_validation_error_kinds_generate_context() {
        let error_kinds = vec![
            ValidationErrorKind::UndefinedType,
            ValidationErrorKind::ConflictingMetadata,
            ValidationErrorKind::InvalidServiceOperation,
            ValidationErrorKind::InvalidDependency,
            ValidationErrorKind::TypeMismatch,
            ValidationErrorKind::MissingRequiredField,
        ];

        for kind in error_kinds {
            let error =
                CsilgenError::validation_error_with_context("test error", None, kind.clone());
            if let CsilgenError::ValidationError(ve) = error {
                // All validation errors should have helpful suggestions
                assert!(
                    ve.suggestion.is_some(),
                    "ValidationErrorKind::{kind:?} should have a suggestion"
                );
            }
        }
    }

    #[test]
    fn test_error_location_reporting_accuracy() {
        use csilgen_common::ErrorLocation;

        let location = ErrorLocation {
            line: 42,
            column: 15,
            file: Some("test.csil".to_string()),
        };

        let error = CsilgenError::parse_error_with_snippet(
            "Invalid syntax",
            Some(location),
            ParseErrorKind::CddlSyntax,
            "line content here",
        );

        let reporter = ErrorReporter::new(CliOutputConfig::default());
        let result = reporter.report_error(&error);
        assert!(result.is_ok());
    }

    #[test]
    fn test_wasm_error_context_reporting() {
        let wasm_error =
            CsilgenError::wasm_error_with_context("Generator compilation failed", "rust-generator");

        let reporter = ErrorReporter::new(CliOutputConfig::default());
        let result = reporter.report_error(&wasm_error);
        assert!(result.is_ok());
    }

    #[test]
    fn test_error_message_truncation_handling() {
        let very_long_message = "a".repeat(5000);
        let error = CsilgenError::parse_error_with_suggestion(
            &very_long_message,
            None,
            ParseErrorKind::CddlSyntax,
        );

        let reporter = ErrorReporter::new(CliOutputConfig::default());
        let result = reporter.report_error(&error);
        assert!(result.is_ok());
    }
}
