//! Cross-language interop orchestrator (`cargo run -p xtask interop`).
//!
//! For each requested language it generates the interop package, builds that
//! language's harness, then runs the full matrix: every client language against
//! every server language over a per-server loopback port (TCP for rpc/events, UDP
//! for datagrams), for each transport, and reports per-cell pass/fail. See
//! tests/interop/README.md.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, PartialEq)]
enum BuildKind {
    Cargo,
    Go,
    None,
    /// A self-describing harness: `build.sh` builds it and `run <mode> <transport>
    /// <socket>` runs it (the script owns its own toolchain env). Used by every
    /// language past the rust/go/python reference trio so wiring is uniform.
    Script,
}

#[derive(Clone)]
struct Lang {
    name: &'static str,
    gen_target: &'static str,
    package_name: &'static str,
    harness_dir: &'static str,
    gen_subdir: &'static str,
    build: BuildKind,
    /// The loopback port this language's SERVER binds (clients connect here). One per
    /// language; reused across the three transports, which run sequentially.
    port: u16,
}

/// All languages with a harness. Add an entry as each language's harness lands.
fn registry() -> Vec<Lang> {
    vec![
        Lang {
            name: "rust",
            gen_target: "rust",
            package_name: "interop_api",
            harness_dir: "tests/interop/harness/rust",
            gen_subdir: "gen",
            build: BuildKind::Cargo,
            port: 6387,
        },
        Lang {
            name: "go",
            gen_target: "go",
            package_name: "interopapi",
            harness_dir: "tests/interop/harness/go",
            gen_subdir: "gen",
            build: BuildKind::Go,
            port: 6388,
        },
        Lang {
            name: "python",
            gen_target: "python",
            package_name: "interop_api",
            harness_dir: "tests/interop/harness/python",
            gen_subdir: "gen",
            build: BuildKind::None,
            port: 6389,
        },
        script_lang("typescript", 6390),
        script_lang("java", 6391),
        script_lang("csharp", 6392),
        script_lang("c", 6393),
        script_lang("ruby", 6394),
        script_lang("elixir", 6395),
        script_lang("dart", 6396),
        script_lang("ocaml", 6397),
        script_lang("zig", 6398),
        script_lang("kotlin", 6399),
        // 6400 is reserved for swift. It is excluded from the matrix for now (no
        // `swiftc` toolchain in this environment); when one is available, add
        // `script_lang("swift", 6400)` here and create tests/interop/harness/swift/.
    ]
}

/// A language whose harness follows the uniform `build.sh` + `run` convention.
fn script_lang(name: &'static str, port: u16) -> Lang {
    // harness_dir leaks the name via a leaked String so it can stay `&'static str`
    // in Lang; the registry is built once per process.
    let dir: &'static str = Box::leak(format!("tests/interop/harness/{name}").into_boxed_str());
    Lang {
        name,
        gen_target: name,
        package_name: "interop_api",
        harness_dir: dir,
        gen_subdir: "gen",
        build: BuildKind::Script,
        port,
    }
}

impl Lang {
    fn dir(&self, root: &Path) -> PathBuf {
        root.join(self.harness_dir)
    }
    fn gen_dest(&self, root: &Path) -> PathBuf {
        self.dir(root).join(self.gen_subdir)
    }

    /// The argv prefix + env that launches this harness; `server`/`client`,
    /// transport, and socket are appended by the caller.
    fn launch(&self, root: &Path) -> (Vec<String>, Vec<(String, String)>) {
        let dir = self.dir(root);
        match self.name {
            "rust" => (
                vec![
                    dir.join("target/debug/csil-interop")
                        .to_string_lossy()
                        .into_owned(),
                ],
                vec![],
            ),
            "go" => (
                vec![dir.join("csil-interop").to_string_lossy().into_owned()],
                vec![],
            ),
            "python" => {
                let pp = format!(
                    "{}:{}",
                    self.gen_dest(root).display(),
                    root.join("transports/python").display()
                );
                (
                    vec![
                        "python3".to_string(),
                        dir.join("main.py").to_string_lossy().into_owned(),
                    ],
                    vec![("PYTHONPATH".to_string(), pp)],
                )
            }
            _ => (vec![dir.join("run").to_string_lossy().into_owned()], vec![]),
        }
    }
}

fn run_in(dir: &Path, program: &str, args: &[&str]) -> Result<()> {
    let ok = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("failed to launch {program} in {}", dir.display()))?
        .success();
    if !ok {
        bail!("`{program} {}` failed in {}", args.join(" "), dir.display());
    }
    Ok(())
}

/// Generate the interop package for one language into its harness `gen/` dir.
fn generate(root: &Path, cli: &Path, lang: &Lang) -> Result<()> {
    let spec = root.join("tests/interop/interop.csil");
    let spec_body =
        std::fs::read_to_string(&spec).with_context(|| format!("reading {}", spec.display()))?;
    let opts = format!(
        "options {{ emit_packages: [\"{}\"], package_name: \"{}\", package_version: \"0.1.0\" }}\n\n",
        lang.gen_target, lang.package_name
    );
    let gen_input_dir = root.join("target/interop");
    std::fs::create_dir_all(&gen_input_dir)?;
    let gen_input = gen_input_dir.join(format!("gen-input-{}.csil", lang.name));
    std::fs::write(&gen_input, format!("{opts}{spec_body}"))?;

    let dest = lang.gen_dest(root);
    let _ = std::fs::remove_dir_all(&dest);
    let ok = Command::new(cli)
        .args(["generate", "--input"])
        .arg(&gen_input)
        .args(["--target", lang.gen_target, "--output"])
        .arg(&dest)
        .status()
        .context("running csilgen generate")?
        .success();
    if !ok {
        bail!("csilgen generate failed for {}", lang.name);
    }
    Ok(())
}

fn build_lang(root: &Path, lang: &Lang) -> Result<()> {
    let dir = lang.dir(root);
    println!("  building {} harness…", lang.name);
    match lang.build {
        BuildKind::Cargo => run_in(&dir, "cargo", &["build", "--quiet"]),
        BuildKind::Go => run_in(&dir, "go", &["build", "-o", "csil-interop", "."]),
        BuildKind::None => Ok(()),
        BuildKind::Script => {
            // Make the convention scripts executable, then build via build.sh.
            for f in ["build.sh", "run"] {
                let p = dir.join(f);
                if let Ok(meta) = std::fs::metadata(&p) {
                    let mut perm = meta.permissions();
                    use std::os::unix::fs::PermissionsExt;
                    perm.set_mode(0o755);
                    let _ = std::fs::set_permissions(&p, perm);
                }
            }
            run_in(&dir, "bash", &["build.sh"])
        }
    }
}

/// Spawn a server and wait until it prints `READY` on stdout (or time out).
fn spawn_server(
    argv: &[String],
    env: &[(String, String)],
    transport: &str,
    port: u16,
) -> Result<Child> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.arg("server").arg(transport).arg(port.to_string());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("spawning server")?;

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut line = String::new();
    loop {
        line.clear();
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("server did not become READY within timeout");
        }
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            let _ = child.kill();
            bail!("server exited before READY");
        }
        if line.trim() == "READY" {
            break;
        }
    }
    // Drain remaining stdout in the background so the pipe never blocks the server.
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = reader.read_to_end(&mut sink);
    });
    Ok(child)
}

/// Run a client and parse its JSON result into (all_ok, failed-case details).
fn run_client(
    argv: &[String],
    env: &[(String, String)],
    transport: &str,
    port: u16,
) -> (bool, Vec<String>) {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.arg("client").arg(transport).arg(port.to_string());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => return (false, vec![format!("spawn failed: {e}")]),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'));
    let line = match json_line {
        Some(l) => l,
        None => {
            let err = String::from_utf8_lossy(&out.stderr);
            return (
                false,
                vec![format!("no JSON result (stderr: {})", err.trim())],
            );
        }
    };
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return (false, vec![format!("bad JSON: {e}")]),
    };
    let mut failures = Vec::new();
    if let Some(cases) = v.get("cases").and_then(|c| c.as_array()) {
        if cases.is_empty() {
            failures.push("no cases run".to_string());
        }
        for c in cases {
            let ok = c.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
            if !ok {
                let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                let detail = c.get("detail").and_then(|d| d.as_str()).unwrap_or("");
                failures.push(format!("{name}: {detail}"));
            }
        }
    } else {
        failures.push("malformed result (no cases array)".to_string());
    }
    (failures.is_empty(), failures)
}

pub fn run(langs_filter: Vec<String>, transports_filter: Vec<String>) -> Result<()> {
    let root = std::env::current_dir()?;
    let cli = root.join("target/release/csilgen");

    println!("== building csilgen CLI (release) ==");
    run_in(
        &root,
        "cargo",
        &["build", "--release", "--quiet", "-p", "csilgen"],
    )?;
    if !cli.exists() {
        bail!("csilgen binary not found at {}", cli.display());
    }

    let all = registry();
    let langs: Vec<Lang> = if langs_filter.is_empty() {
        all
    } else {
        all.into_iter()
            .filter(|l| langs_filter.iter().any(|f| f == l.name))
            .collect()
    };
    if langs.is_empty() {
        bail!("no known languages selected");
    }
    let transports: Vec<String> = if transports_filter.is_empty() {
        vec!["rpc".into(), "events".into(), "datagrams".into()]
    } else {
        transports_filter
    };

    println!(
        "== interop: langs [{}] transports [{}] ==",
        langs.iter().map(|l| l.name).collect::<Vec<_>>().join(", "),
        transports.join(", ")
    );

    // Phase 1: generate + build each harness.
    for lang in &langs {
        println!("== preparing {} ==", lang.name);
        generate(&root, &cli, lang)?;
        build_lang(&root, lang)?;
    }

    // Phase 2: run the matrix. The server binds its own loopback port; every client
    // connects to that port. One port per server-lang, reused across transports
    // (they run sequentially; harnesses set SO_REUSEADDR for clean rebind).
    // results[(transport, client, server)] = (ok, failures)
    let mut results: Vec<(String, String, String, bool, Vec<String>)> = Vec::new();
    for transport in &transports {
        for server in &langs {
            let (sargv, senv) = server.launch(&root);
            let mut child = match spawn_server(&sargv, &senv, transport, server.port) {
                Ok(c) => c,
                Err(e) => {
                    // Mark every client cell against this server as failed.
                    for client in &langs {
                        results.push((
                            transport.clone(),
                            client.name.to_string(),
                            server.name.to_string(),
                            false,
                            vec![format!("server start: {e}")],
                        ));
                    }
                    continue;
                }
            };
            for client in &langs {
                let (cargv, cenv) = client.launch(&root);
                let (ok, failures) = run_client(&cargv, &cenv, transport, server.port);
                results.push((
                    transport.clone(),
                    client.name.to_string(),
                    server.name.to_string(),
                    ok,
                    failures,
                ));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    // Phase 3: report.
    report(&langs, &transports, &results)
}

fn report(
    langs: &[Lang],
    transports: &[String],
    results: &[(String, String, String, bool, Vec<String>)],
) -> Result<()> {
    let cell = |transport: &str, client: &str, server: &str| {
        results
            .iter()
            .find(|(t, c, s, _, _)| t == transport && c == client && s == server)
            .map(|(_, _, _, ok, _)| *ok)
    };

    let mut total = 0usize;
    let mut passed = 0usize;
    for transport in transports {
        println!("\n=== {transport} ===  (rows = client, cols = server)");
        let header: Vec<&str> = langs.iter().map(|l| l.name).collect();
        print!("{:>10} |", "");
        for h in &header {
            print!(" {h:>8}");
        }
        println!();
        for client in langs {
            print!("{:>10} |", client.name);
            for server in langs {
                let mark = match cell(transport, client.name, server.name) {
                    Some(true) => "✓",
                    Some(false) => "✗",
                    None => "·",
                };
                print!(" {mark:>8}");
                total += 1;
                if matches!(cell(transport, client.name, server.name), Some(true)) {
                    passed += 1;
                }
            }
            println!();
        }
    }

    // Failure detail.
    let failures: Vec<_> = results.iter().filter(|(_, _, _, ok, _)| !ok).collect();
    if !failures.is_empty() {
        println!("\n=== failures ===");
        for (t, c, s, _, details) in &failures {
            println!("[{t}] client={c} server={s}");
            for d in details.iter() {
                println!("    - {d}");
            }
        }
    }

    println!("\n== interop summary: {passed}/{total} cells passed ==");
    if passed != total {
        bail!("interop matrix has {} failing cell(s)", total - passed);
    }
    Ok(())
}
