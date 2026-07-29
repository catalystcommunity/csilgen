"""Runnerlib lifecycle jobs for the csilgen repository."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import tarfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import List, Mapping, Sequence

from src.logging import log_stdout
from src.plugins import Plugin, PluginContext, PluginPhase


GENERATOR_PACKAGES = (
    "csilgen-c-generator",
    "csilgen-csharp-generator",
    "csilgen-dart-generator",
    "csilgen-elixir-generator",
    "csilgen-go-generator",
    "csilgen-java-generator",
    "csilgen-json-generator",
    "csilgen-kotlin-generator",
    "csilgen-ocaml-generator",
    "csilgen-openapi-generator",
    "csilgen-php-generator",
    "csilgen-python-generator",
    "csilgen-ruby-generator",
    "csilgen-rust-generator",
    "csilgen-swift-generator",
    "csilgen-typescript-generator",
    "csilgen-zig-generator",
)

TRANSPORTS = (
    "c",
    "csharp",
    "dart",
    "elixir",
    "go",
    "java",
    "kotlin",
    "ocaml",
    "php",
    "python",
    "ruby",
    "rust",
    "swift",
    "typescript",
    "zig",
)

CONVENTIONAL_COMMIT = re.compile(
    r"^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)"
    r"(\([^)]+\))?!?: .+"
)


def _repo_root(context: PluginContext) -> Path:
    configured = Path(context.config.code_dir)
    if configured.exists():
        return configured.resolve()
    source_path = context.metadata.get("source_path")
    if source_path:
        return Path(source_path).resolve()
    return Path("/job/src")


def _run(
    args: Sequence[str],
    *,
    cwd: Path,
    env: Mapping[str, str] | None = None,
    capture: bool = False,
    sensitive: Sequence[str] = (),
) -> subprocess.CompletedProcess[str]:
    printable = " ".join(args)
    for value in sensitive:
        printable = printable.replace(value, "[REDACTED]")
    log_stdout(f"+ {printable}")
    command_env = os.environ.copy()
    if env:
        command_env.update(env)
    return subprocess.run(
        list(args),
        cwd=cwd,
        env=command_env,
        check=True,
        shell=False,
        text=True,
        capture_output=capture,
    )


def _cargo(
    args: Sequence[str],
    root: Path,
    *,
    env: Mapping[str, str] | None = None,
) -> None:
    _run(("cargo", *args), cwd=root, env=env)


def _build_wasm(root: Path, *, release: bool) -> None:
    _run(("rustup", "target", "add", "wasm32-unknown-unknown"), cwd=root)
    args: List[str] = ["build", "--target", "wasm32-unknown-unknown"]
    if release:
        args.append("--release")
    for package in (*GENERATOR_PACKAGES, "csilgen-noop-generator", "csilgen-simple-test"):
        args.extend(("--package", package))
    _cargo(args, root)


def _validate_conventional_commits(root: Path) -> None:
    diff_base = os.environ.get("REACTORCIDE_DIFF_BASE")
    if not diff_base:
        candidate = _run(
            ("git", "merge-base", "HEAD", "origin/main"),
            cwd=root,
            capture=True,
        )
        diff_base = candidate.stdout.strip()

    result = _run(
        ("git", "log", f"{diff_base}..HEAD", "--pretty=format:%H%x00%s"),
        cwd=root,
        capture=True,
    )
    failures = []
    for line in result.stdout.splitlines():
        commit_hash, _, subject = line.partition("\x00")
        if CONVENTIONAL_COMMIT.fullmatch(subject):
            log_stdout(f"OK: {subject}")
        else:
            failures.append(f"{commit_hash[:12]} {subject}")
    if failures:
        details = "\n".join(failures)
        raise RuntimeError(
            "Commit subjects must use Conventional Commits. Invalid commits:\n"
            f"{details}"
        )


def _test_core(root: Path) -> None:
    if not Path("/usr/include/openssl/ssl.h").exists():
        _run(("apt-get", "update"), cwd=root)
        _run(
            (
                "apt-get",
                "install",
                "-y",
                "--no-install-recommends",
                "libssl-dev",
            ),
            cwd=root,
        )
    _run(("rustup", "target", "add", "wasm32-unknown-unknown"), cwd=root)
    _run(("rustup", "component", "add", "rustfmt", "clippy"), cwd=root)
    _cargo(
        (
            "build",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--package",
            "csilgen-noop-generator",
            "--package",
            "csilgen-simple-test",
        ),
        root,
    )
    _cargo(("fmt", "--all", "--", "--check"), root)
    _cargo(("clippy", "--workspace", "--all-targets", "--", "-D", "warnings"), root)
    test_home = root / "target" / "ci-home"
    generator_dir = test_home / ".csilgen" / "generators"
    generator_dir.mkdir(parents=True, exist_ok=True)
    wasm_dir = root / "target" / "wasm32-unknown-unknown" / "release"
    for name in ("csilgen_noop_generator.wasm", "csilgen_simple_test.wasm"):
        shutil.copy2(wasm_dir / name, generator_dir / name)
    cargo_home = os.environ.get(
        "CARGO_HOME",
        str(Path.home() / ".cargo"),
    )
    prefetch = root / "target" / "ci-prefetch"
    (prefetch / "src").mkdir(parents=True, exist_ok=True)
    (prefetch / "Cargo.toml").write_text(
        "[package]\n"
        'name = "csilgen-ci-prefetch"\n'
        'version = "0.0.0"\n'
        'edition = "2024"\n'
        "\n[dependencies]\n"
        'ureq = "2"\n'
        "\n[workspace]\n",
        encoding="utf-8",
    )
    (prefetch / "src" / "main.rs").write_text(
        "fn main() {}\n",
        encoding="utf-8",
    )
    _cargo(
        ("fetch", "--manifest-path", str(prefetch / "Cargo.toml")),
        root,
        env={"CARGO_HOME": cargo_home},
    )
    _cargo(
        ("test", "--workspace"),
        root,
        env={
            "CARGO_HOME": cargo_home,
            "HOME": str(test_home),
        },
    )
    _run(
        (
            "python3",
            "-m",
            "unittest",
            "discover",
            "-s",
            ".reactorcide/tests",
        ),
        cwd=root,
    )


def _test_generators(root: Path) -> None:
    _build_wasm(root, release=True)


def _test_transports(root: Path) -> None:
    conformance = str(root / "transports" / "conformance")
    c_build = root / "transports" / "c" / "build-test"
    if c_build.exists():
        shutil.rmtree(c_build)
    commands = (
        (root, ("cargo", "test", "-p", "csilgen-transport"), None),
        (root / "transports/go", ("go", "test", "./..."), None),
        (
            root / "transports/typescript",
            ("npm", "test", "--silent"),
            None,
        ),
        (
            root / "transports/python",
            ("python3", "-m", "unittest", "discover", "-s", "tests"),
            None,
        ),
        (
            root / "transports/java",
            ("./gradlew", "--no-daemon", "test"),
            None,
        ),
        (
            root / "transports/csharp",
            ("dotnet", "test", "Csilgen.Transport.sln", "--nologo"),
            None,
        ),
        (
            root,
            (
                "cmake",
                "-S",
                "transports/c",
                "-B",
                "transports/c/build-test",
                "-DCMAKE_BUILD_TYPE=Debug",
                "-DBUILD_TESTING=ON",
                "-DCMAKE_C_FLAGS=-fsanitize=address,undefined "
                "-fno-sanitize-recover=all",
            ),
            None,
        ),
        (
            root,
            ("cmake", "--build", "transports/c/build-test"),
            None,
        ),
        (
            root,
            (
                "ctest",
                "--test-dir",
                "transports/c/build-test",
                "--output-on-failure",
            ),
            None,
        ),
        (root / "transports/swift", ("swift", "test"), None),
        (
            root / "transports/kotlin",
            ("./gradlew", "--no-daemon", "test", "--quiet"),
            None,
        ),
        (root / "transports/zig", ("zig", "build", "test"), None),
        (
            root / "transports/ocaml",
            ("dune", "build"),
            {"CSILGEN_CONFORMANCE_DIR": conformance},
        ),
        (
            root / "transports/ocaml",
            ("dune", "runtest", "--force"),
            {"CSILGEN_CONFORMANCE_DIR": conformance},
        ),
        (root / "transports/elixir", ("mix", "deps.get"), None),
        (root / "transports/elixir", ("mix", "test"), None),
        (
            root / "transports/ruby",
            ("ruby", "-Ilib", "-Itest", "test/all_tests.rb"),
            None,
        ),
        (root / "transports/dart", ("dart", "pub", "get"), None),
        (root / "transports/dart", ("dart", "test"), None),
        (
            root / "transports/php",
            ("php", "tests/conformance_test.php"),
            None,
        ),
        (
            root / "transports/php",
            ("php", "tests/roundtrip_test.php"),
            None,
        ),
        (
            root / "transports/php",
            ("php", "tests/max_frame_test.php"),
            None,
        ),
    )
    for cwd, args, env in commands:
        _run(args, cwd=cwd, env=env)


def _test_interop(root: Path) -> None:
    started = time.monotonic()
    _cargo(
        ("run", "-p", "xtask", "--", "interop"),
        root,
        env={"GOFLAGS": "-buildvcs=false"},
    )
    elapsed = time.monotonic() - started
    log_stdout(f"Interop runtime: {elapsed:.2f} seconds")
    if elapsed > 360:
        raise RuntimeError(
            f"Interop runtime was {elapsed:.2f} seconds. The limit is 360 seconds."
        )


def _ci_image_name(root: Path) -> str:
    dockerfile = root / ".reactorcide" / "images" / "csilgen-ci" / "Dockerfile"
    digest = hashlib.sha256()
    tracked = _run(
        (
            "git",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ),
        cwd=root,
        capture=True,
    )
    for relative in sorted(
        path for path in tracked.stdout.split("\x00") if path
    ):
        source = root / relative
        if not source.is_file():
            continue
        digest.update(relative.encode("utf-8"))
        digest.update(b"\x00")
        digest.update(source.read_bytes())
        digest.update(b"\x00")
    return f"csilgen-ci:{digest.hexdigest()[:12]}"


def _container_runtime() -> str:
    docker = shutil.which("docker")
    if docker:
        return docker
    if os.environ.get("DOCKER_HOST"):
        return _install_docker_client()
    podman = shutil.which("podman")
    if podman:
        return podman
    raise RuntimeError("Docker or Podman is required for this job")


def _install_docker_client() -> str:
    version = "27.5.1"
    checksums = {
        "aarch64": (
            "e6b53725a73763ab3f988c73f8772eae"
            "d429754c1a579db5ff11f21990fd1817"
        ),
        "x86_64": (
            "4f798b3ee1e0140eab5bf30b0edc4e84"
            "f4cdb53255a429dc3bbae9524845d640"
        ),
    }
    architectures = {
        "aarch64": "aarch64",
        "x86_64": "x86_64",
    }
    architecture = architectures.get(platform.machine())
    if not architecture:
        raise RuntimeError(
            f"Docker CLI is not available for {platform.machine()}"
        )

    install_root = Path("/tmp") / f"csilgen-docker-{version}"
    binary = install_root / "docker" / "docker"
    if binary.exists():
        return str(binary)

    install_root.mkdir(parents=True, exist_ok=True)
    archive = install_root / "docker.tgz"
    url = (
        "https://download.docker.com/linux/static/stable/"
        f"{architecture}/docker-{version}.tgz"
    )
    log_stdout(f"Download Docker CLI {version} for {architecture}")
    urllib.request.urlretrieve(url, archive)
    actual_checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    if actual_checksum != checksums[architecture]:
        archive.unlink(missing_ok=True)
        raise RuntimeError("The Docker CLI archive checksum is invalid")
    with tarfile.open(archive, "r:gz") as package:
        package.extractall(install_root, filter="data")
    if not binary.exists():
        raise RuntimeError("The Docker CLI archive did not contain docker")
    return str(binary)


def _ensure_ci_image(root: Path) -> str:
    image = _ci_image_name(root)
    runtime = _container_runtime()
    inspected = subprocess.run(
        (runtime, "image", "inspect", image),
        cwd=root,
        shell=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if inspected.returncode != 0:
        _run(
            (
                runtime,
                "build",
                "--tag",
                image,
                "--file",
                ".reactorcide/images/csilgen-ci/Dockerfile",
                ".",
            ),
            cwd=root,
        )
    return image


def _run_in_ci_image(root: Path, job_name: str) -> None:
    image = _ensure_ci_image(root)
    runtime = _container_runtime()
    _run(
        (
            runtime,
            "run",
            "--rm",
            "--user",
            "root",
            "--workdir",
            "/job/src",
            "--env",
            f"CSILGEN_JOB={job_name}",
            "--env",
            "CSILGEN_TOOLCHAIN_READY=1",
            "--entrypoint",
            "runnerlib",
            image,
            "run",
            "--plugin-dir",
            "/job/src/.reactorcide/plugins",
            "--job-command",
            "true",
        ),
        cwd=root,
    )


def _tar_tracked(
    output: Path,
    root: Path,
    members: Sequence[str],
) -> None:
    result = _run(
        ("git", "ls-files", "-z", "--", *members),
        cwd=root,
        capture=True,
    )
    paths = tuple(path for path in result.stdout.split("\x00") if path)
    with tarfile.open(output, "w:gz", format=tarfile.PAX_FORMAT) as archive:
        for path in paths:
            archive.add(root / path, arcname=path)


def _tar_files(output: Path, files: Sequence[tuple[Path, str]]) -> None:
    with tarfile.open(output, "w:gz", format=tarfile.PAX_FORMAT) as archive:
        for source, archive_name in files:
            archive.add(source, arcname=archive_name)


def _docker_build(
    root: Path,
    image: str,
    target: str,
    command: Sequence[str],
    binary: str,
) -> Path:
    runtime = _container_runtime()
    build_root = root / "target" / "release-container-builds"
    build_root.mkdir(parents=True, exist_ok=True)
    safe_target = re.sub(r"[^a-zA-Z0-9_.-]", "-", target)
    dockerfile = build_root / f"{safe_target}.Dockerfile"
    build_command = [
        *command,
        "--release",
        "--package",
        "csilgen",
        "--target",
        target,
    ]
    dockerfile.write_text(
        f"FROM {image}\n"
        "WORKDIR /io\n"
        "COPY . /io\n"
        f"RUN {json.dumps(build_command)}\n",
        encoding="utf-8",
    )
    digest = hashlib.sha256(
        dockerfile.read_bytes() + target.encode("utf-8")
    ).hexdigest()[:12]
    build_image = f"csilgen-release-{safe_target}:{digest}"
    _run(
        (
            runtime,
            "build",
            "--tag",
            build_image,
            "--file",
            str(dockerfile),
            ".",
        ),
        cwd=root,
    )

    destination = build_root / safe_target / Path(binary).name
    destination.parent.mkdir(parents=True, exist_ok=True)
    container = f"csilgen-release-copy-{uuid.uuid4().hex[:12]}"
    _run((runtime, "create", "--name", container, build_image), cwd=root)
    try:
        _run(
            (
                runtime,
                "cp",
                f"{container}:/io/{binary}",
                str(destination),
            ),
            cwd=root,
        )
    finally:
        subprocess.run(
            (runtime, "rm", "--force", container),
            cwd=root,
            shell=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    return destination


def _build_release_artifacts(root: Path, version: str) -> Path:
    output = root / "target" / "release-artifacts"
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)

    zig_image = "ghcr.io/rust-cross/cargo-zigbuild:latest"
    builds = (
        (
            "linux-x86_64",
            zig_image,
            "x86_64-unknown-linux-gnu.2.28",
            ("cargo", "zigbuild"),
            "target/x86_64-unknown-linux-gnu/release/csilgen",
            "csilgen",
        ),
        (
            "linux-aarch64",
            zig_image,
            "aarch64-unknown-linux-gnu.2.28",
            ("cargo", "zigbuild"),
            "target/aarch64-unknown-linux-gnu/release/csilgen",
            "csilgen",
        ),
        (
            "darwin-aarch64",
            zig_image,
            "aarch64-apple-darwin",
            ("cargo", "zigbuild"),
            "target/aarch64-apple-darwin/release/csilgen",
            "csilgen",
        ),
        (
            "windows-x86_64",
            zig_image,
            "x86_64-pc-windows-gnu",
            ("cargo", "zigbuild"),
            "target/x86_64-pc-windows-gnu/release/csilgen.exe",
            "csilgen.exe",
        ),
    )
    for platform, image, target, command, binary, archive_name in builds:
        built_binary = _docker_build(
            root,
            image,
            target,
            command,
            binary,
        )
        archive = output / f"csilgen-{version}-{platform}.tar.gz"
        _tar_files(
            archive,
            (
                (built_binary, archive_name),
                (root / "LICENSE", "LICENSE"),
                (root / "README.md", "README.md"),
            ),
        )

    _build_wasm(root, release=True)
    wasm_dir = root / "target" / "wasm32-unknown-unknown" / "release"
    for package in GENERATOR_PACKAGES:
        target = package.removeprefix("csilgen-").removesuffix("-generator")
        wasm = wasm_dir / package.replace("-", "_")
        wasm = wasm.with_suffix(".wasm")
        archive = output / f"csilgen-generator-{target}-{version}.tar.gz"
        _tar_files(
            archive,
            (
                (wasm, wasm.name),
                (root / "LICENSE", "LICENSE"),
            ),
        )

    for language in TRANSPORTS:
        archive = output / f"csilgen-transport-{language}-{version}.tar.gz"
        _tar_tracked(
            archive,
            root,
            (
                f"transports/{language}",
                "transports/conformance",
                "LICENSE",
            ),
        )
    return output


def _semver_tags(root: Path, *, dry_run: bool) -> dict:
    binary = shutil.which("semver-tags")
    if not binary:
        _run(
            ("go", "install", "github.com/catalystcommunity/semver-tags@v0.4.0"),
            cwd=root,
        )
        go_path = _run(("go", "env", "GOPATH"), cwd=root, capture=True)
        binary = str(Path(go_path.stdout.strip()) / "bin" / "semver-tags")
    args = [binary, "run", "--output_json", "--branch", "HEAD:main"]
    if dry_run:
        args.append("--dry_run")
    result = _run(args, cwd=root, capture=True)
    start = result.stdout.find("{")
    if start < 0:
        raise RuntimeError("semver-tags did not return JSON output")
    return json.loads(result.stdout[start:])


def _configure_git_auth(root: Path, token: str, repository: str) -> None:
    credential = base64.b64encode(
        f"x-access-token:{token}".encode("utf-8")
    ).decode("ascii")
    _run(
        (
            "git",
            "config",
            "--local",
            "http.https://github.com/.extraheader",
            f"AUTHORIZATION: basic {credential}",
        ),
        cwd=root,
        sensitive=(credential,),
    )
    _run(
        (
            "git",
            "config",
            "--local",
            "remote.origin.pushurl",
            f"https://github.com/{repository}.git",
        ),
        cwd=root,
    )


def _clear_git_auth(root: Path) -> None:
    for key in (
        "http.https://github.com/.extraheader",
        "remote.origin.pushurl",
    ):
        subprocess.run(
            ("git", "config", "--local", "--unset-all", key),
            cwd=root,
            shell=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


def _github_request(
    method: str,
    url: str,
    token: str,
    *,
    body: bytes | None = None,
    content_type: str = "application/json",
) -> dict:
    request = urllib.request.Request(url, data=body, method=method)
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Accept", "application/vnd.github+json")
    request.add_header("X-GitHub-Api-Version", "2022-11-28")
    if body is not None:
        request.add_header("Content-Type", content_type)
    with urllib.request.urlopen(request) as response:
        payload = response.read()
    return json.loads(payload) if payload else {}


def _publish_github_release(
    root: Path,
    token: str,
    repository: str,
    tag: str,
    notes: str,
    artifacts: Path,
) -> None:
    api = f"https://api.github.com/repos/{repository}"
    encoded_tag = urllib.parse.quote(tag, safe="")
    try:
        release = _github_request(
            "GET",
            f"{api}/releases/tags/{encoded_tag}",
            token,
        )
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        payload = json.dumps(
            {
                "tag_name": tag,
                "name": f"csilgen {tag}",
                "body": notes,
                "draft": False,
                "prerelease": False,
            }
        ).encode("utf-8")
        release = _github_request(
            "POST",
            f"{api}/releases",
            token,
            body=payload,
        )

    upload_url = release["upload_url"].split("{", 1)[0]
    existing = {asset["name"]: asset["id"] for asset in release.get("assets", [])}
    for artifact in sorted(artifacts.glob("*.tar.gz")):
        if artifact.name in existing:
            _github_request(
                "DELETE",
                f"{api}/releases/assets/{existing[artifact.name]}",
                token,
            )
        query = urllib.parse.urlencode({"name": artifact.name})
        _github_request(
            "POST",
            f"{upload_url}?{query}",
            token,
            body=artifact.read_bytes(),
            content_type="application/gzip",
        )
        log_stdout(f"Uploaded {artifact.name}")


def _release(root: Path, *, publish: bool) -> None:
    repository = os.environ.get(
        "REACTORCIDE_REPO", "catalystcommunity/csilgen"
    )
    _run(
        (
            "git",
            "fetch",
            "--tags",
            "--force",
            f"https://github.com/{repository}.git",
        ),
        cwd=root,
    )
    preview = _semver_tags(root, dry_run=True)
    if preview["New_release_published"] != "true":
        if publish:
            tags = _run(
                (
                    "git",
                    "tag",
                    "--points-at",
                    "HEAD",
                    "--list",
                    "v*",
                    "--sort=-version:refname",
                ),
                cwd=root,
                capture=True,
            )
            tag = next(
                (line for line in tags.stdout.splitlines() if line),
                None,
            )
            if tag:
                token = os.environ.get("GITHUB_PAT")
                if not token:
                    raise RuntimeError("GITHUB_PAT is required for a release")
                artifacts = _build_release_artifacts(
                    root,
                    tag.removeprefix("v"),
                )
                _publish_github_release(
                    root,
                    token,
                    repository,
                    tag,
                    preview["New_release_notes"],
                    artifacts,
                )
                return
        log_stdout("No release is required.")
        return

    version = preview["New_release_version"]
    artifacts = _build_release_artifacts(root, version)
    if not publish:
        log_stdout(f"Release dry run created artifacts in {artifacts}")
        return

    token = os.environ.get("GITHUB_PAT")
    if not token:
        raise RuntimeError("GITHUB_PAT is required for a release")

    _configure_git_auth(root, token, repository)
    try:
        result = _semver_tags(root, dry_run=False)
    finally:
        _clear_git_auth(root)

    tag = result["New_release_git_tag"]
    notes = result["New_release_notes"]
    _publish_github_release(root, token, repository, tag, notes, artifacts)


class CsilgenJobsPlugin(Plugin):
    """Run the selected csilgen job after runnerlib prepares the source."""

    def __init__(self) -> None:
        super().__init__(name="csilgen_jobs", priority=100)

    def supported_phases(self) -> List[PluginPhase]:
        return [PluginPhase.POST_SOURCE_PREP]

    def execute(self, context: PluginContext) -> None:
        root = _repo_root(context)
        config_count = int(os.environ.get("GIT_CONFIG_COUNT", "0"))
        os.environ[f"GIT_CONFIG_KEY_{config_count}"] = "safe.directory"
        os.environ[f"GIT_CONFIG_VALUE_{config_count}"] = str(root)
        os.environ["GIT_CONFIG_COUNT"] = str(config_count + 1)
        job = os.environ.get("CSILGEN_JOB")
        if not job:
            raise RuntimeError("CSILGEN_JOB must select a runnerlib lifecycle job")

        if job == "conventional-commits":
            _validate_conventional_commits(root)
        elif job == "core":
            _test_core(root)
        elif job == "generators":
            _test_generators(root)
        elif job in {"transports", "interop"} and not os.environ.get(
            "CSILGEN_TOOLCHAIN_READY"
        ):
            _run_in_ci_image(root, job)
        elif job == "transports":
            _test_transports(root)
        elif job == "interop":
            _test_interop(root)
        elif job == "package":
            _release(root, publish=False)
        elif job == "release":
            _release(root, publish=True)
        else:
            raise RuntimeError(f"Unknown CSILGEN_JOB value: {job}")
