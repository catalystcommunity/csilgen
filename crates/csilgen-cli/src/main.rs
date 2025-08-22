//! CLI tool for generating code from CBOR Service Interface Language (CSIL) interface definitions

pub mod error_reporting;

use crate::error_reporting::{CliOutputConfig, ErrorReporter, format_generation_summary};
use clap::{Parser, Subcommand};
use csilgen::generate_code_with_progress;
use csilgen_core::{
    CsilSpec, FormatConfig, ImportResolver, LintConfig, detect_breaking_changes_from_files,
    format_directory, lint_directory, parse_csil_file, parse_csil_file_streaming,
    validate_spec_optimized,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "csilgen")]
#[command(about = "A CLI tool for generating code from CSIL interface definitions")]
struct Cli {
    /// Enable verbose output for debugging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Suppress non-essential output for scripting
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse and validate a CSIL file
    Validate {
        /// Path to the CSIL file
        #[arg(short, long)]
        input: PathBuf,
    },
    /// Generate code from CSIL file(s)
    Generate {
        /// Path to CSIL file, directory, or glob pattern (e.g., "user.csil", "./schemas", "*.csil", "schemas/**/*.csil")
        ///
        /// For directories/patterns with multiple files, csilgen performs dependency analysis to:
        /// - Identify entry point files (not imported by others)
        /// - Generate code only from entry points to avoid duplicates
        /// - Include dependencies automatically via import resolution
        ///   
        ///   Use CSIL_VERBOSE=1 for detailed dependency analysis output
        #[arg(short, long)]
        input: String,
        /// Target generator (json, rust, python, typescript, openapi)
        #[arg(short, long)]
        target: String,
        /// Output directory
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Compare two CSIL files for breaking changes
    Breaking {
        /// Path to the current CSIL file
        #[arg(long)]
        current: PathBuf,
        /// Path to the new CSIL file
        #[arg(long)]
        new: PathBuf,
    },
    /// Format CSIL files in a directory
    Format {
        /// Path to directory containing CSIL files
        path: PathBuf,
        /// Show what would be formatted without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Lint CSIL files in a directory
    Lint {
        /// Path to directory containing CSIL files
        path: PathBuf,
        /// Automatically fix linting issues where possible
        #[arg(long)]
        fix: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Create CLI output configuration
    let output_config = CliOutputConfig {
        use_colors: !cli.no_color && atty::is(atty::Stream::Stderr),
        verbose: cli.verbose,
        quiet: cli.quiet,
    };

    let reporter = ErrorReporter::new(output_config.clone());

    match run_command(&cli, &reporter) {
        Ok(_) => Ok(()),
        Err(e) => {
            reporter.report_error(&e)?;
            std::process::exit(if e.is_fatal() { 1 } else { 0 });
        }
    }
}

fn run_command(cli: &Cli, reporter: &ErrorReporter) -> Result<(), csilgen_common::CsilgenError> {
    match &cli.command {
        Commands::Validate { input } => {
            reporter.report_info(&format!("Validating CSIL file: {}", input.display()))?;

            let spec = parse_csil_with_imports(input).map_err(|e| {
                csilgen_common::CsilgenError::parse_error_with_suggestion(
                    &e.to_string(),
                    None,
                    csilgen_common::ParseErrorKind::CddlSyntax,
                )
            })?;

            reporter.report_success(&format!("Parsed {} rules successfully", spec.rules.len()))?;
            // Use optimized validation for large specs
            if spec.rules.len() > 1000 {
                validate_spec_optimized(&spec)?;
                reporter.report_success(&format!(
                    "Large spec validated successfully using parallel validation ({} rules)",
                    spec.rules.len()
                ))?;
            } else {
                validate_spec_optimized(&spec)?;
                reporter.report_success("CSIL file validated successfully")?;
            }
        }
        Commands::Generate {
            input,
            target,
            output,
        } => {
            reporter.report_info(&format!("Generating {target} code from: {input}"))?;
            reporter.report_debug(&format!("Output directory: {}", output.display()))?;

            let pb = if !reporter.config.quiet {
                Some(reporter.create_progress_bar(0, "Generating code"))
            } else {
                None
            };

            match generate_code_with_progress(input, target, output, pb) {
                Ok(result) => {
                    let summary = format_generation_summary(
                        result.processed_files,
                        result.generated_files,
                        result.total_size,
                        result.error_count,
                        reporter.config.use_colors,
                    );

                    if !reporter.config.quiet {
                        println!("\n{summary}");
                    }

                    if result.error_count > 0 {
                        return Err(csilgen_common::CsilgenError::GenerationError(format!(
                            "Generation completed with {} error(s)",
                            result.error_count
                        )));
                    }

                    if result.generated_files > 0 {
                        reporter
                            .report_info(&format!("Output written to: {}", output.display()))?;
                    }
                }
                Err(e) => {
                    return Err(csilgen_common::CsilgenError::GenerationError(e.to_string()));
                }
            }
        }
        Commands::Breaking { current, new } => {
            reporter.report_info(&format!(
                "Checking for breaking changes between {} and {}",
                current.display(),
                new.display()
            ))?;

            let pb = reporter.create_progress_bar(2, "Analyzing changes");
            pb.inc(1);

            match detect_breaking_changes_from_files(current, new) {
                Ok(report) => {
                    pb.finish_and_clear();

                    if report.has_breaking_changes {
                        reporter.report_error(&csilgen_common::CsilgenError::ValidationError(
                            csilgen_common::ValidationError {
                                message: "Breaking changes detected".to_string(),
                                location: None,
                                suggestion: Some(
                                    "Review the changes and update dependent code accordingly"
                                        .to_string(),
                                ),
                                error_kind: csilgen_common::ValidationErrorKind::TypeMismatch,
                            },
                        ))?;

                        for change in &report.breaking_changes {
                            reporter.report_warning(&format!("{change:?}"))?;
                        }

                        return Err(csilgen_common::CsilgenError::ValidationError(
                            csilgen_common::ValidationError {
                                message: format!(
                                    "{} breaking change(s) found",
                                    report.breaking_changes.len()
                                ),
                                location: None,
                                suggestion: None,
                                error_kind: csilgen_common::ValidationErrorKind::TypeMismatch,
                            },
                        ));
                    } else {
                        reporter.report_success("No breaking changes detected")?;
                        if !report.non_breaking_changes.is_empty() {
                            reporter.report_info("Non-breaking changes found:")?;
                            for change in &report.non_breaking_changes {
                                reporter.report_debug(&format!("+ {change}"))?;
                            }
                        }
                    }
                }
                Err(e) => {
                    pb.finish_and_clear();
                    return Err(csilgen_common::CsilgenError::IoError(e.to_string()));
                }
            }
        }
        Commands::Format { path, dry_run } => {
            let config = FormatConfig::default();

            if *dry_run {
                reporter.report_info(&format!(
                    "Would format CSIL files in: {} (dry run)",
                    path.display()
                ))?;
            } else {
                reporter.report_info(&format!("Formatting CSIL files in: {}", path.display()))?;
            }

            match format_directory(path, &config, *dry_run) {
                Ok(results) => {
                    let pb = reporter.create_progress_bar(results.len() as u64, "Formatting files");
                    let mut changed_count = 0;

                    for (file_path, result) in &results {
                        pb.inc(1);
                        if result.changed {
                            changed_count += 1;
                            if *dry_run {
                                reporter.report_debug(&format!("Would format: {file_path}"))?;
                            } else {
                                reporter.report_debug(&format!("Formatted: {file_path}"))?;
                            }
                        }
                    }

                    pb.finish_and_clear();

                    if changed_count == 0 {
                        reporter.report_success("All files are already formatted correctly")?;
                    } else {
                        reporter.report_success(&format!("Processed {changed_count} file(s)"))?;
                    }
                }
                Err(e) => {
                    return Err(csilgen_common::CsilgenError::IoError(e.to_string()));
                }
            }
        }
        Commands::Lint { path, fix } => {
            let config = LintConfig::default();

            if *fix {
                reporter.report_info(&format!(
                    "Linting and fixing CSIL files in: {}",
                    path.display()
                ))?;
            } else {
                reporter.report_info(&format!("Linting CSIL files in: {}", path.display()))?;
            }

            match lint_directory(path, &config, *fix) {
                Ok(results) => {
                    let pb = reporter.create_progress_bar(results.len() as u64, "Linting files");
                    let mut total_errors = 0;
                    let mut total_warnings = 0;

                    for (file_path, result) in &results {
                        pb.inc(1);
                        if !result.issues.is_empty() {
                            reporter.report_debug(&format!("\n{file_path}:"))?;
                            for issue in &result.issues {
                                let severity_symbol = match issue.severity {
                                    csilgen_core::LintSeverity::Error => "❌",
                                    csilgen_core::LintSeverity::Warning => "⚠️",
                                    csilgen_core::LintSeverity::Info => "ℹ️",
                                };
                                reporter.report_debug(&format!(
                                    "  {} [{}] {}",
                                    severity_symbol, issue.rule_name, issue.message
                                ))?;
                                if let Some(suggestion) = &issue.suggestion {
                                    reporter.report_debug(&format!("      💡 {suggestion}"))?;
                                }
                            }
                        }
                        total_errors += result.error_count;
                        total_warnings += result.warning_count;
                    }

                    pb.finish_and_clear();
                    reporter.report_info(&format!(
                        "Summary: {total_errors} error(s), {total_warnings} warning(s)"
                    ))?;

                    if total_errors > 0 {
                        return Err(csilgen_common::CsilgenError::ValidationError(
                            csilgen_common::ValidationError {
                                message: format!("Linting found {total_errors} error(s)"),
                                location: None,
                                suggestion: Some(
                                    "Fix the errors and run linting again".to_string(),
                                ),
                                error_kind:
                                    csilgen_common::ValidationErrorKind::MissingRequiredField,
                            },
                        ));
                    }
                }
                Err(e) => {
                    return Err(csilgen_common::CsilgenError::IoError(e.to_string()));
                }
            }
        }
    }

    Ok(())
}

/// Parse a CSIL file with import resolution
fn parse_csil_with_imports(path: &std::path::Path) -> anyhow::Result<CsilSpec> {
    let mut resolver = ImportResolver::new();

    // Use streaming parser for large files to optimize memory usage
    let file_size = std::fs::metadata(path)?.len();
    let mut spec = if file_size > 10 * 1024 * 1024 {
        // >10MB
        parse_csil_file_streaming(path)?
    } else {
        parse_csil_file(path)?
    };

    resolver.resolve_imports(&mut spec, path)?;
    Ok(spec)
}
