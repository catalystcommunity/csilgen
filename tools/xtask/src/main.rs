//! Development automation tasks for csilgen

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Development automation tasks for csilgen")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build all crates
    Build,
    /// Run tests for all crates
    Test,
    /// Run clippy linting
    Clippy,
    /// Format all code
    Fmt,
    /// Build WASM modules
    BuildWasm,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Build => {
            println!("Building all crates...");
            std::process::Command::new("cargo")
                .args(["build", "--workspace"])
                .status()?;
        }
        Commands::Test => {
            println!("Running tests...");
            std::process::Command::new("cargo")
                .args(["test", "--workspace"])
                .status()?;
        }
        Commands::Clippy => {
            println!("Running clippy...");
            std::process::Command::new("cargo")
                .args([
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ])
                .status()?;
        }
        Commands::Fmt => {
            println!("Formatting code...");
            std::process::Command::new("cargo")
                .args(["fmt", "--all"])
                .status()?;
        }
        Commands::BuildWasm => {
            println!("Building WASM modules...");
            std::process::Command::new("cargo")
                .args([
                    "build",
                    "--target",
                    "wasm32-unknown-unknown",
                    "--release",
                    "--package",
                    "csilgen-noop-generator",
                    "--package",
                    "csilgen-json-generator",
                    "--package",
                    "csilgen-rust-generator",
                    "--package",
                    "csilgen-typescript-generator",
                ])
                .status()?;
        }
    }

    Ok(())
}
