"""Build and run the csilgen cross-language interoperability matrix."""

from __future__ import annotations

import argparse
import json
import os
import selectors
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Dict, List, Mapping, NamedTuple, Sequence, Tuple


class Language(NamedTuple):
    name: str
    generator: str
    package: str
    port: int


LANGUAGES = (
    Language("rust", "rust", "interop_api", 6387),
    Language("go", "go", "interopapi", 6388),
    Language("python", "python", "interop_api", 6389),
    Language("typescript", "typescript", "interop_api", 6390),
    Language("java", "java", "interop_api", 6391),
    Language("csharp", "csharp", "interop_api", 6392),
    Language("c", "c", "interop_api", 6393),
    Language("ruby", "ruby", "interop_api", 6394),
    Language("elixir", "elixir", "interop_api", 6395),
    Language("dart", "dart", "interop_api", 6396),
    Language("ocaml", "ocaml", "interop_api", 6397),
    Language("zig", "zig", "interop_api", 6398),
    Language("kotlin", "kotlin", "interop_api", 6399),
)
TRANSPORTS = ("rpc", "events", "datagrams")


def _run(
    args: Sequence[str | Path],
    cwd: Path,
    *,
    env: Mapping[str, str] | None = None,
    capture: bool = False,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    command = tuple(str(arg) for arg in args)
    print("+ " + " ".join(command), flush=True)
    command_env = os.environ.copy()
    if env:
        command_env.update(env)
    return subprocess.run(
        command,
        cwd=cwd,
        env=command_env,
        check=True,
        shell=False,
        text=True,
        capture_output=capture,
        timeout=timeout,
    )


def _harness(root: Path, language: Language) -> Path:
    return root / "tests" / "interop" / "harness" / language.name


def _generated(root: Path, language: Language) -> Path:
    return _harness(root, language) / "gen"


def _build_generators(root: Path, languages: Sequence[Language]) -> None:
    packages = tuple(
        dict.fromkeys(
            f"csilgen-{language.generator}-generator"
            for language in languages
        )
    )
    print("== building interop WASM generators (release) ==", flush=True)
    args = [
        "cargo",
        "build",
        "--release",
        "--quiet",
        "--target",
        "wasm32-unknown-unknown",
    ]
    for package in packages:
        args.extend(("--package", package))
    _run(args, root)


def _generate(root: Path, cli: Path, language: Language) -> None:
    specification = root / "tests" / "interop" / "interop.csil"
    options = (
        "options { emit_packages: "
        f'["{language.generator}"], package_name: "{language.package}", '
        'package_version: "0.1.0" }\n\n'
    )
    input_dir = root / "target" / "interop"
    input_dir.mkdir(parents=True, exist_ok=True)
    generated_input = input_dir / f"gen-input-{language.name}.csil"
    generated_input.write_text(
        options + specification.read_text(encoding="utf-8"),
        encoding="utf-8",
    )
    destination = _generated(root, language)
    if destination.exists():
        shutil.rmtree(destination)
    _run(
        (
            cli,
            "generate",
            "--input",
            generated_input,
            "--target",
            language.generator,
            "--output",
            destination,
        ),
        root,
    )


def _build_ruby(root: Path, harness: Path) -> None:
    for require_path, gem_name in (("bigdecimal", "bigdecimal"), ("json", "json")):
        available = subprocess.run(
            ("ruby", "-e", f'require "{require_path}"'),
            cwd=harness,
            shell=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if available.returncode != 0:
            _run(
                ("gem", "install", "--user-install", "--no-document", gem_name),
                harness,
            )
    _run(
        (
            "ruby",
            f"-I{harness / 'gen' / 'lib'}",
            f"-I{root / 'transports' / 'ruby' / 'lib'}",
            "-e",
            'require "interop_api"; require "csilgen/transport"',
        ),
        harness,
    )


def _build_ocaml(root: Path, harness: Path) -> None:
    transport = harness / "transport"
    if transport.exists():
        shutil.rmtree(transport)
    transport.mkdir(parents=True)
    library = root / "transports" / "ocaml" / "lib"
    for pattern in ("*.ml", "*.mli"):
        for source in sorted(library.glob(pattern)):
            shutil.copy2(source, transport / source.name)
    (transport / "dune").write_text(
        "(library\n (name csilgen_transport)\n (libraries unix))\n",
        encoding="utf-8",
    )
    _run(("dune", "build", "--profile", "release"), harness)


def _build(root: Path, language: Language) -> None:
    harness = _harness(root, language)
    generated = _generated(root, language)
    print(f"  building {language.name} harness...", flush=True)
    if language.name == "rust":
        _run(("cargo", "build", "--quiet"), harness)
    elif language.name == "go":
        _run(
            ("go", "build", "-o", "csil-interop", "."),
            harness,
            env={"GOFLAGS": "-buildvcs=false"},
        )
    elif language.name == "python":
        return
    elif language.name == "typescript":
        _run(("npm", "install", "--no-audit", "--no-fund", "--silent"), generated)
        _run((generated / "node_modules" / ".bin" / "tsc",), generated)
    elif language.name == "java":
        classes = harness / "classes"
        if classes.exists():
            shutil.rmtree(classes)
        classes.mkdir()
        sources = sorted((root / "transports" / "java" / "src" / "main" / "java").rglob("*.java"))
        sources.extend(sorted((generated / "src" / "main" / "java").rglob("*.java")))
        _run(("javac", "-d", classes, *sources, harness / "Main.java"), harness)
    elif language.name == "csharp":
        _run(
            ("dotnet", "build", "csil-interop.csproj", "-c", "Release", "--nologo"),
            harness,
        )
    elif language.name == "c":
        sources = sorted((root / "transports" / "c" / "src").glob("*.c"))
        _run(
            (
                os.environ.get("CC", "cc"),
                "-std=c11",
                "-O2",
                "-Wall",
                "-Wextra",
                f"-I{root / 'transports' / 'c' / 'include'}",
                f"-I{root / 'transports' / 'c' / 'src'}",
                f"-I{harness}",
                harness / "main.c",
                *sources,
                "-o",
                harness / "csil-interop-c",
            ),
            harness,
        )
    elif language.name == "ruby":
        _build_ruby(root, harness)
    elif language.name == "elixir":
        mix_env = {"MIX_ENV": "dev"}
        transport = root / "transports" / "elixir"
        _run(("mix", "deps.get"), transport, env=mix_env)
        _run(("mix", "compile"), transport, env=mix_env)
        _run(("mix", "deps.get"), generated, env=mix_env)
        _run(("mix", "compile"), generated, env=mix_env)
    elif language.name == "dart":
        _run(("dart", "pub", "get"), harness)
    elif language.name == "ocaml":
        _build_ocaml(root, harness)
    elif language.name == "zig":
        _run(("zig", "build"), harness)
    elif language.name == "kotlin":
        _run(("gradle", "--no-daemon", "-q", "installDist"), harness)
    else:
        raise RuntimeError(f"No build command is registered for {language.name}")
    print(f"{language.name} harness built", flush=True)


def _launch(
    root: Path,
    language: Language,
) -> Tuple[Tuple[str, ...], Path, Dict[str, str]]:
    harness = _harness(root, language)
    generated = _generated(root, language)
    if language.name == "rust":
        return ((str(harness / "target" / "debug" / "csil-interop"),), harness, {})
    if language.name == "go":
        return ((str(harness / "csil-interop"),), harness, {})
    if language.name == "python":
        python_path = os.pathsep.join(
            (str(generated), str(root / "transports" / "python"))
        )
        return (("python3", str(harness / "main.py")), harness, {"PYTHONPATH": python_path})
    if language.name == "typescript":
        return (("node", str(harness / "main.ts")), harness, {})
    if language.name == "java":
        return (("java", "-cp", str(harness / "classes"), "interop.Main"), harness, {})
    if language.name == "csharp":
        binary = harness / "bin" / "Release" / "net8.0" / "csil-interop.dll"
        return (("dotnet", str(binary)), harness, {})
    if language.name == "c":
        return ((str(harness / "csil-interop-c"),), harness, {})
    if language.name == "ruby":
        return (
            (
                "ruby",
                f"-I{generated / 'lib'}",
                f"-I{root / 'transports' / 'ruby' / 'lib'}",
                str(harness / "main.rb"),
            ),
            harness,
            {},
        )
    if language.name == "elixir":
        transport_ebin = root / "transports" / "elixir" / "_build" / "dev" / "lib" / "csilgen_transport" / "ebin"
        generated_ebin = generated / "_build" / "dev" / "lib" / "interop_api" / "ebin"
        return (
            (
                "elixir",
                "-pa",
                str(generated_ebin),
                "-pa",
                str(transport_ebin),
                str(harness / "main.exs"),
            ),
            harness,
            {},
        )
    if language.name == "dart":
        return (("dart", "run", "main.dart"), harness, {})
    if language.name == "ocaml":
        return ((str(harness / "_build" / "default" / "main.exe"),), harness, {})
    if language.name == "zig":
        return ((str(harness / "zig-out" / "bin" / "csil-interop-zig"),), harness, {})
    if language.name == "kotlin":
        distribution = harness / "build" / "install" / "csil-interop-kotlin"
        return (
            (
                "java",
                "-cp",
                str(distribution / "lib" / "*"),
                "csil.interop.MainKt",
            ),
            harness,
            {},
        )
    raise RuntimeError(f"No launch command is registered for {language.name}")


def _process_environment(values: Mapping[str, str]) -> Dict[str, str]:
    environment = os.environ.copy()
    environment.update(values)
    return environment


def _drain(stream: object) -> None:
    if stream is None:
        return
    for _ in stream:  # type: ignore[union-attr]
        pass


def _spawn_server(
    root: Path,
    language: Language,
    transport: str,
) -> subprocess.Popen[str]:
    command, cwd, values = _launch(root, language)
    process = subprocess.Popen(
        (*command, "server", transport, str(language.port)),
        cwd=cwd,
        env=_process_environment(values),
        shell=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if process.stdout is None:
        process.kill()
        raise RuntimeError("The server did not provide a readiness stream")
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + 20
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                process.kill()
                process.wait()
                raise RuntimeError("server did not become READY within timeout")
            line = process.stdout.readline()
            if not line:
                process.wait()
                raise RuntimeError("server exited before READY")
            if line.strip() == "READY":
                break
    finally:
        selector.close()
    threading.Thread(target=_drain, args=(process.stdout,), daemon=True).start()
    return process


def _run_client(
    root: Path,
    language: Language,
    transport: str,
    port: int,
) -> Tuple[bool, List[str]]:
    command, cwd, values = _launch(root, language)
    try:
        result = subprocess.run(
            (*command, "client", transport, str(port)),
            cwd=cwd,
            env=_process_environment(values),
            shell=False,
            text=True,
            capture_output=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return False, [f"client failed: {error}"]
    json_line = next(
        (line for line in reversed(result.stdout.splitlines()) if line.lstrip().startswith("{")),
        None,
    )
    if json_line is None:
        return False, [f"no JSON result (stderr: {result.stderr.strip()})"]
    try:
        payload = json.loads(json_line)
    except json.JSONDecodeError as error:
        return False, [f"bad JSON: {error}"]
    cases = payload.get("cases")
    if not isinstance(cases, list):
        return False, ["malformed result (no cases array)"]
    if not cases:
        return False, ["no cases run"]
    failures = []
    for case in cases:
        if not isinstance(case, dict) or case.get("ok") is not True:
            name = case.get("name", "?") if isinstance(case, dict) else "?"
            detail = case.get("detail", "") if isinstance(case, dict) else ""
            failures.append(f"{name}: {detail}")
    return not failures, failures


Result = Tuple[str, str, str, bool, List[str]]


def _report(
    languages: Sequence[Language],
    transports: Sequence[str],
    results: Sequence[Result],
) -> None:
    by_cell = {(item[0], item[1], item[2]): item for item in results}
    total = 0
    passed = 0
    for transport in transports:
        print(f"\n=== {transport} ===  (rows = client, cols = server)")
        print(f"{'':>10} |", end="")
        for language in languages:
            print(f" {language.name:>8}", end="")
        print()
        for client in languages:
            print(f"{client.name:>10} |", end="")
            for server in languages:
                item = by_cell.get((transport, client.name, server.name))
                mark = "✓" if item and item[3] else "✗" if item else "·"
                print(f" {mark:>8}", end="")
                total += 1
                if item and item[3]:
                    passed += 1
            print()
    failures = [item for item in results if not item[3]]
    if failures:
        print("\n=== failures ===")
        for transport, client, server, _, details in failures:
            print(f"[{transport}] client={client} server={server}")
            for detail in details:
                print(f"    - {detail}")
    print(f"\n== interop summary: {passed}/{total} cells passed ==")
    if passed != total:
        raise RuntimeError(f"interop matrix has {total - passed} failing cell(s)")


def _select_languages(names: Sequence[str]) -> Tuple[Language, ...]:
    if not names:
        return LANGUAGES
    unknown = sorted(set(names) - {language.name for language in LANGUAGES})
    if unknown:
        raise RuntimeError("Unknown interop languages: " + ", ".join(unknown))
    return tuple(language for language in LANGUAGES if language.name in names)


def _select_transports(names: Sequence[str]) -> Tuple[str, ...]:
    if not names:
        return TRANSPORTS
    unknown = sorted(set(names) - set(TRANSPORTS))
    if unknown:
        raise RuntimeError("Unknown interop transports: " + ", ".join(unknown))
    return tuple(transport for transport in TRANSPORTS if transport in names)


def run(
    root: Path,
    language_names: Sequence[str] = (),
    transport_names: Sequence[str] = (),
) -> None:
    root = root.resolve()
    languages = _select_languages(language_names)
    transports = _select_transports(transport_names)
    cli = root / "target" / "release" / "csilgen"
    _build_generators(root, languages)
    print("== building csilgen CLI (release) ==", flush=True)
    _run(("cargo", "build", "--release", "--quiet", "-p", "csilgen"), root)
    if not cli.is_file():
        raise RuntimeError(f"csilgen binary not found at {cli}")
    print(
        "== interop: langs "
        f"[{', '.join(language.name for language in languages)}] "
        f"transports [{', '.join(transports)}] ==",
        flush=True,
    )
    for language in languages:
        print(f"== preparing {language.name} ==", flush=True)
        _generate(root, cli, language)
        _build(root, language)

    results: List[Result] = []
    for transport in transports:
        for server in languages:
            try:
                process = _spawn_server(root, server, transport)
            except RuntimeError as error:
                for client in languages:
                    results.append(
                        (transport, client.name, server.name, False, [f"server start: {error}"])
                    )
                continue
            try:
                for client in languages:
                    ok, failures = _run_client(
                        root,
                        client,
                        transport,
                        server.port,
                    )
                    results.append((transport, client.name, server.name, ok, failures))
            finally:
                process.kill()
                process.wait()
    _report(languages, transports, results)


def _split_values(value: str) -> Tuple[str, ...]:
    return tuple(item.strip() for item in value.split(",") if item.strip())


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--langs", default="")
    parser.add_argument("--transports", default="")
    args = parser.parse_args(argv)
    try:
        run(
            args.root,
            _split_values(args.langs),
            _split_values(args.transports),
        )
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"interop failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
