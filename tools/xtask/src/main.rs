//! Development automation tasks for csilgen

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
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
    /// Build and install WASM modules to ~/.csilgen/generators/
    InstallWasm,
}

fn build_wasm() -> Result<()> {
    println!("Building WASM modules...");
    let status = std::process::Command::new("cargo")
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
            "--package",
            "csilgen-go",
        ])
        .status()
        .context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("WASM build failed");
    }
    Ok(())
}

fn install_wasm() -> Result<()> {
    build_wasm()?;

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let generators_dir = home.join(".csilgen/generators");

    fs::create_dir_all(&generators_dir)
        .with_context(|| format!("Failed to create {}", generators_dir.display()))?;

    let wasm_source = PathBuf::from("target/wasm32-unknown-unknown/release");

    let mut installed = Vec::new();
    for entry in fs::read_dir(&wasm_source)
        .with_context(|| format!("Failed to read {}", wasm_source.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("wasm")
            && let Some(name) = path.file_name()
        {
            let name_str = name.to_string_lossy();
            if name_str.starts_with("csilgen_") {
                let dest = generators_dir.join(name);
                fs::copy(&path, &dest).with_context(|| {
                    format!("Failed to copy {} to {}", path.display(), dest.display())
                })?;
                installed.push(name_str.to_string());
            }
        }
    }

    println!("Installed {} WASM modules to {}:", installed.len(), generators_dir.display());
    for name in &installed {
        println!("  {name}");
    }

    Ok(())
}

fn main() -> Result<()> {
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
            build_wasm()?;
        }
        Commands::InstallWasm => {
            install_wasm()?;
        }
    }

    Ok(())
}
