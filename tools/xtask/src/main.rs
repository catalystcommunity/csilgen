//! Development automation tasks for csilgen

use std::fs;
use std::path::{Path, PathBuf};

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
    /// Run the transport reference-library tests for all four languages, checking
    /// each against the shared conformance vectors. Languages whose toolchain is
    /// absent are skipped with a message rather than failing the run.
    TestTransports,
    /// Call the trusted Python implementation of the cross-language interop
    /// matrix. See tests/interop/README.md.
    Interop {
        /// Restrict clients to these languages (default: all registered).
        #[arg(long, value_delimiter = ',')]
        langs: Vec<String>,
        /// Restrict servers to these languages (default: selected clients).
        #[arg(long, value_delimiter = ',')]
        servers: Vec<String>,
        /// Restrict to these transports: rpc, events, datagrams (default: all).
        #[arg(long, value_delimiter = ',')]
        transports: Vec<String>,
    },
}

/// True if `cmd --version` (or `version`) runs successfully — used to detect an
/// available language toolchain before invoking its test runner.
fn toolchain_present(cmd: &str, version_arg: &str) -> bool {
    std::process::Command::new(cmd)
        .arg(version_arg)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_transport_test(dir: &Path, runner: &str, args: &[&str]) -> Result<bool> {
    let mut cmd = std::process::Command::new(runner);
    cmd.args(args).current_dir(dir);

    cmd.status()
        .with_context(|| format!("failed to launch {runner} for {}", dir.display()))
        .map(|status| status.success())
}

fn test_transports() -> Result<()> {
    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    // Rust is always available (we are running under cargo).
    println!("== Rust transport tests ==");
    let rust_ok = std::process::Command::new("cargo")
        .args(["test", "-p", "csilgen-transport"])
        .status()?
        .success();
    if rust_ok {
        ran.push("rust");
    } else {
        failed.push("rust");
    }

    // (language, toolchain cmd, version arg, project dir, runner program, runner args).
    type LangTest = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static [&'static str],
    );
    let langs: &[LangTest] = &[
        (
            "go",
            "go",
            "version",
            "transports/go",
            "go",
            &["test", "./..."],
        ),
        (
            "typescript",
            "node",
            "--version",
            "transports/typescript",
            "npm",
            &["test", "--silent"],
        ),
        (
            "python",
            "python3",
            "--version",
            "transports/python",
            "python3",
            &["-m", "unittest", "discover", "-s", "tests"],
        ),
        // New-language transports each ship a `run-tests.sh` that owns their build tool's
        // multi-step setup+build+test (CMake configure/build/ctest, gradle wrapper,
        // `dart pub get` then test, opam env, …) behind one command, so they slot into
        // this uniform runner. The presence probe is the lightest toolchain check that
        // exits 0 when the language is installed.
        (
            "java",
            "java",
            "-version",
            "transports/java",
            "./run-tests.sh",
            &[],
        ),
        (
            "csharp",
            "dotnet",
            "--version",
            "transports/csharp",
            "./run-tests.sh",
            &[],
        ),
        (
            "c",
            "cmake",
            "--version",
            "transports/c",
            "./run-tests.sh",
            &[],
        ),
        (
            "swift",
            "swift",
            "--version",
            "transports/swift",
            "./run-tests.sh",
            &[],
        ),
        (
            "kotlin",
            "java",
            "-version",
            "transports/kotlin",
            "./run-tests.sh",
            &[],
        ),
        (
            "zig",
            "zig",
            "version",
            "transports/zig",
            "./run-tests.sh",
            &[],
        ),
        (
            "ocaml",
            "dune",
            "--version",
            "transports/ocaml",
            "./run-tests.sh",
            &[],
        ),
        (
            "elixir",
            "elixir",
            "--version",
            "transports/elixir",
            "./run-tests.sh",
            &[],
        ),
        (
            "ruby",
            "ruby",
            "--version",
            "transports/ruby",
            "./run-tests.sh",
            &[],
        ),
        (
            "dart",
            "dart",
            "--version",
            "transports/dart",
            "./run-tests.sh",
            &[],
        ),
        (
            "php",
            "php",
            "--version",
            "transports/php",
            "./run-tests.sh",
            &[],
        ),
    ];

    for (lang, tool, ver, dir, runner, args) in langs {
        let path = PathBuf::from(dir);
        if !path.exists() {
            skipped.push(format!("{lang} (no {dir})"));
            continue;
        }
        if !toolchain_present(tool, ver) {
            skipped.push(format!("{lang} ({tool} toolchain not found)"));
            continue;
        }
        println!("\n== {lang} transport tests ==");
        let ok = run_transport_test(&path, runner, args)?;
        if ok {
            ran.push(lang);
        } else {
            failed.push(lang);
        }
    }

    println!("\n== transport test summary ==");
    println!("  ran:     {}", ran.join(", "));
    if !skipped.is_empty() {
        println!("  skipped: {}", skipped.join(", "));
    }
    if !failed.is_empty() {
        anyhow::bail!("transport tests failed for: {}", failed.join(", "));
    }
    Ok(())
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
            "csilgen-go-generator",
            "--package",
            "csilgen-python-generator",
            "--package",
            "csilgen-openapi-generator",
            "--package",
            "csilgen-java-generator",
            "--package",
            "csilgen-csharp-generator",
            "--package",
            "csilgen-c-generator",
            "--package",
            "csilgen-swift-generator",
            "--package",
            "csilgen-kotlin-generator",
            "--package",
            "csilgen-zig-generator",
            "--package",
            "csilgen-ocaml-generator",
            "--package",
            "csilgen-elixir-generator",
            "--package",
            "csilgen-ruby-generator",
            "--package",
            "csilgen-dart-generator",
            "--package",
            "csilgen-php-generator",
            // Not a target — a tiny wasm fixture loaded directly by the
            // wasm-generators integration tests. Building it here keeps those
            // tests runnable without a manual `cargo build --target wasm32` step.
            "--package",
            "csilgen-simple-test",
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

    println!(
        "Installed {} WASM modules to {}:",
        installed.len(),
        generators_dir.display()
    );
    for name in &installed {
        println!("  {name}");
    }

    Ok(())
}

fn run_interop(langs: &[String], servers: &[String], transports: &[String]) -> Result<()> {
    let root = std::env::current_dir()?;
    let script = root.join(".reactorcide/scripts/interop.py");
    let mut command = std::process::Command::new("python3");
    command.arg(&script).arg("--root").arg(&root);
    if !langs.is_empty() {
        command.arg("--langs").arg(langs.join(","));
    }
    if !servers.is_empty() {
        command.arg("--servers").arg(servers.join(","));
    }
    if !transports.is_empty() {
        command.arg("--transports").arg(transports.join(","));
    }
    let status = command
        .status()
        .context("Failed to run the trusted Python interop implementation")?;
    if !status.success() {
        anyhow::bail!("interoperability tests failed");
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
        Commands::TestTransports => {
            test_transports()?;
        }
        Commands::Interop {
            langs,
            servers,
            transports,
        } => {
            run_interop(langs, servers, transports)?;
        }
    }

    Ok(())
}
