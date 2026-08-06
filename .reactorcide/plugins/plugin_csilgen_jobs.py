"""Runnerlib lifecycle jobs for the csilgen repository."""

from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, List, Mapping, NamedTuple, Sequence

from src.logging import log_stdout
from src.plugins import Plugin, PluginContext, PluginPhase


CI_TOOLCHAIN_DOCKERFILE = """\
FROM docker.io/library/node:24-bookworm-slim AS node
FROM mcr.microsoft.com/dotnet/sdk:8.0-bookworm-slim AS dotnet
FROM docker.io/library/dart:stable AS dart
FROM docker.io/library/swift:6.0-bookworm AS swift
FROM docker.io/library/eclipse-temurin:17-jdk AS java
FROM docker.io/library/gradle:8.14.3-jdk17 AS gradle
FROM docker.io/library/debian:bookworm-slim AS zig

RUN apt-get update \\
    && apt-get install -y --no-install-recommends ca-certificates xz-utils \\
    && rm -rf /var/lib/apt/lists/*
ADD https://ziglang.org/download/0.14.1/zig-x86_64-linux-0.14.1.tar.xz /tmp/zig.tar.xz
RUN echo "24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c  /tmp/zig.tar.xz" \\
        | sha256sum --check --strict \\
    && mkdir /opt/zig \\
    && tar -xJf /tmp/zig.tar.xz --strip-components=1 -C /opt/zig

FROM containers.catalystsquad.com/public/reactorcide/runnerbase:latest

USER root

RUN apt-get update \\
    && apt-get install -y --no-install-recommends \\
        cmake \\
        elixir \\
        erlang-dev \\
        libicu76 \\
        ocaml-dune \\
        ocaml \\
        php-cli \\
        ruby-full \\
    && rm -rf /var/lib/apt/lists/*

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \\
    && ln -s /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

COPY --from=dotnet /usr/share/dotnet /usr/share/dotnet
RUN ln -s /usr/share/dotnet/dotnet /usr/local/bin/dotnet

COPY --from=dart /usr/lib/dart /usr/lib/dart
RUN ln -s /usr/lib/dart/bin/dart /usr/local/bin/dart

COPY --from=swift /usr/bin/clang* /usr/bin/
COPY --from=swift /usr/bin/lld* /usr/bin/
COPY --from=swift /usr/bin/llvm* /usr/bin/
COPY --from=swift /usr/bin/sourcekit* /usr/bin/
COPY --from=swift /usr/bin/swift* /usr/bin/
COPY --from=swift /usr/include/swift /usr/include/swift
COPY --from=swift /usr/lib/clang /usr/lib/clang
COPY --from=swift /usr/lib/libIndexStore.so* /usr/lib/
COPY --from=swift /usr/lib/swift /usr/lib/swift
COPY --from=swift /usr/lib/swift_static /usr/lib/swift_static
COPY --from=swift /usr/libexec/swift /usr/libexec/swift
COPY --from=swift /usr/share/swift /usr/share/swift

COPY --from=zig /opt/zig /usr/local/zig
RUN ln -s /usr/local/zig/zig /usr/local/bin/zig
COPY --from=java /opt/java/openjdk /opt/java/openjdk
COPY --from=gradle /opt/gradle /opt/gradle
ENV JAVA_HOME=/opt/java/openjdk
ENV PATH="/opt/gradle/bin:${JAVA_HOME}/bin:${PATH}"

RUN gem install --no-document json minitest \\
    && rustup target add wasm32-unknown-unknown \\
    && mkdir -p /job/src /job/ci
"""


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

CORE_RELEASE = "csilgen-core"
GENERATOR_RELEASES = {
    f"generator-{package.removeprefix('csilgen-').removesuffix('-generator')}": package
    for package in GENERATOR_PACKAGES
}
TRANSPORT_RELEASES = {
    f"transport-{language}": language for language in TRANSPORTS
}
RELEASE_PACKAGES = (
    CORE_RELEASE,
    *GENERATOR_RELEASES,
    *TRANSPORT_RELEASES,
)
RELEASE_TAG = re.compile(
    r"^(?P<package>[a-z0-9-]+)/v(?P<version>"
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$"
)
RELEASE_MARKER_PREFIX = "<!-- csilgen-release-source:"


class ReleasePlan(NamedTuple):
    """Describe one release target returned by semver-tags."""

    package: str
    published: bool
    version: str
    tag: str
    source_sha: str
    notes: str

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
    args: Sequence[str | Path],
    *,
    cwd: Path,
    env: Mapping[str, str] | None = None,
    capture: bool = False,
    sensitive: Sequence[str] = (),
) -> subprocess.CompletedProcess[str]:
    command = tuple(str(arg) for arg in args)
    printable = " ".join(command)
    for value in sensitive:
        printable = printable.replace(value, "[REDACTED]")
    log_stdout(f"+ {printable}")
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
    )


def _cargo(
    args: Sequence[str],
    root: Path,
    *,
    env: Mapping[str, str] | None = None,
) -> None:
    _run(("cargo", *args), cwd=root, env=env)


def _build_wasm(
    root: Path,
    *,
    release: bool,
    packages: Sequence[str] | None = None,
) -> None:
    _run(("rustup", "target", "add", "wasm32-unknown-unknown"), cwd=root)
    args: List[str] = ["build", "--target", "wasm32-unknown-unknown"]
    if release:
        args.append("--release")
    selected = packages or (
        *GENERATOR_PACKAGES,
        "csilgen-noop-generator",
        "csilgen-simple-test",
    )
    for package in selected:
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


def _interop_server_names() -> tuple[str, ...]:
    return tuple(
        name.strip()
        for name in os.environ.get("CSILGEN_INTEROP_SERVERS", "").split(",")
        if name.strip()
    )


def _test_interop(root: Path) -> None:
    script_path = (
        Path(__file__).resolve().parents[1] / "scripts" / "interop.py"
    )
    spec = importlib.util.spec_from_file_location(
        "csilgen_trusted_interop",
        script_path,
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("The trusted interoperability script is unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    started = time.monotonic()
    module.run(root, server_names=_interop_server_names())
    elapsed = time.monotonic() - started
    log_stdout(f"Interop runtime: {elapsed:.2f} seconds")


def _ci_image_name() -> str:
    digest = hashlib.sha256(CI_TOOLCHAIN_DOCKERFILE.encode("utf-8"))
    return f"csilgen-ci:{digest.hexdigest()[:12]}"


def _container_runtime() -> str:
    docker = shutil.which("docker")
    if docker:
        return docker
    docker_socket = Path("/var/run/docker.sock")
    if os.environ.get("DOCKER_HOST") or docker_socket.exists():
        if not os.environ.get("DOCKER_HOST"):
            os.environ["DOCKER_HOST"] = f"unix://{docker_socket}"
        return _install_docker_client()
    podman = shutil.which("podman")
    if podman:
        _configure_podman_storage()
        return podman
    raise RuntimeError("Docker or Podman is required for this job")


def _configure_podman_storage() -> None:
    storage_root = Path("/tmp/csilgen-podman")
    run_root = storage_root / "runroot"
    graph_root = storage_root / "graphroot"
    run_root.mkdir(parents=True, exist_ok=True)
    graph_root.mkdir(parents=True, exist_ok=True)
    storage_config = storage_root / "storage.conf"
    storage_config.write_text(
        "[storage]\n"
        'driver = "overlay"\n'
        f'runroot = "{run_root}"\n'
        f'graphroot = "{graph_root}"\n'
        "\n[storage.options.overlay]\n"
        'mount_program = "/usr/bin/fuse-overlayfs"\n',
        encoding="utf-8",
    )
    os.environ["CONTAINERS_STORAGE_CONF"] = str(storage_config)


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
    image = _ci_image_name()
    runtime = _container_runtime()
    inspected = subprocess.run(
        (runtime, "image", "inspect", image),
        cwd=root,
        shell=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if inspected.returncode != 0:
        with tempfile.TemporaryDirectory(prefix="csilgen-ci-image-") as path:
            context = Path(path)
            (context / "Dockerfile").write_text(
                CI_TOOLCHAIN_DOCKERFILE,
                encoding="utf-8",
            )
            _run(
                (
                    runtime,
                    "build",
                    "--tag",
                    image,
                    "--file",
                    context / "Dockerfile",
                    context,
                ),
                cwd=root,
            )
    return image


def _copy_job_source(root: Path, destination: Path) -> None:
    result = _run(
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
    for relative in sorted(path for path in result.stdout.split("\x00") if path):
        source = root / relative
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.is_symlink():
            target.symlink_to(os.readlink(source))
        elif source.is_file():
            shutil.copy2(source, target)


def _forwarded_ci_environment() -> tuple[str, ...]:
    forwarded_environment = []
    for name in ("CSILGEN_INTEROP_SERVERS",):
        if value := os.environ.get(name):
            forwarded_environment.extend(("--env", f"{name}={value}"))
    return tuple(forwarded_environment)


def _run_in_ci_image(root: Path, job_name: str) -> None:
    image = _ensure_ci_image(root)
    runtime = _container_runtime()
    created = _run(
        (
            runtime,
            "create",
            "--user",
            "root",
            "--workdir",
            "/job/src",
            "--env",
            f"REACTORCIDE_CSILGEN_JOB={job_name}",
            "--env",
            "CSILGEN_TOOLCHAIN_READY=1",
            "--env",
            "REACTORCIDE_CI_SOURCE_DIR=/job/ci",
            *_forwarded_ci_environment(),
            "--entrypoint",
            "runnerlib",
            image,
            "run",
            "--job-command",
            "true",
        ),
        cwd=root,
        capture=True,
    )
    container_id = created.stdout.strip()
    if not container_id:
        raise RuntimeError("The CI toolchain container was not created")
    try:
        with tempfile.TemporaryDirectory(prefix="csilgen-ci-source-") as path:
            source_copy = Path(path) / "src"
            source_copy.mkdir()
            _copy_job_source(root, source_copy)
            _run(
                (runtime, "cp", f"{source_copy}/.", f"{container_id}:/job/src"),
                cwd=root,
            )
        trusted_reactorcide = Path(__file__).resolve().parents[1]
        _run(
            (
                runtime,
                "cp",
                trusted_reactorcide,
                f"{container_id}:/job/ci/",
            ),
            cwd=root,
        )
        _run((runtime, "start", "--attach", container_id), cwd=root)
    finally:
        subprocess.run(
            (runtime, "rm", "--force", container_id),
            cwd=root,
            shell=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
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


def _install_buildctl(root: Path) -> Path:
    version = "0.17.3"
    architectures = {
        "aarch64": "arm64",
        "x86_64": "amd64",
    }
    architecture = architectures.get(platform.machine())
    if not architecture:
        raise RuntimeError(
            f"buildctl is not available for {platform.machine()}"
        )
    tool_dir = root / "target" / "reactorcide-tools" / f"buildctl-{version}"
    binary = tool_dir / "buildctl"
    if binary.exists():
        return binary

    tool_dir.mkdir(parents=True, exist_ok=True)
    archive_path = tool_dir / "buildkit.tar.gz"
    url = (
        "https://github.com/moby/buildkit/releases/download/"
        f"v{version}/buildkit-v{version}.linux-{architecture}.tar.gz"
    )
    log_stdout(f"Download buildctl {version} for {architecture}")
    urllib.request.urlretrieve(url, archive_path)
    with tarfile.open(archive_path, "r:gz") as archive:
        member = archive.getmember("bin/buildctl")
        source = archive.extractfile(member)
        if source is None:
            raise RuntimeError("The BuildKit archive did not contain buildctl")
        with binary.open("wb") as output:
            shutil.copyfileobj(source, output)
    binary.chmod(0o755)
    archive_path.unlink(missing_ok=True)
    return binary


def _wait_for_buildkit(buildctl: Path, root: Path) -> None:
    if not os.environ.get("BUILDKIT_HOST"):
        raise RuntimeError("The builder capability did not set BUILDKIT_HOST")
    for _ in range(30):
        result = subprocess.run(
            (str(buildctl), "debug", "info"),
            cwd=root,
            shell=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if result.returncode == 0:
            return
        time.sleep(1)
    raise RuntimeError("The BuildKit sidecar did not become ready")


def _builder_build(
    root: Path,
    image: str,
    target: str,
    command: Sequence[str],
    binary: str,
) -> Path:
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
        f"FROM {image} AS build\n"
        "WORKDIR /io\n"
        "COPY . /io\n"
        f"RUN {json.dumps(build_command)}\n"
        "FROM scratch AS export\n"
        f"COPY --from=build /io/{binary} /{Path(binary).name}\n",
        encoding="utf-8",
    )

    destination = build_root / safe_target / Path(binary).name
    destination.parent.mkdir(parents=True, exist_ok=True)
    buildctl = _install_buildctl(root)
    _wait_for_buildkit(buildctl, root)
    _run(
        (
            str(buildctl),
            "build",
            "--frontend",
            "dockerfile.v0",
            "--local",
            f"context={root}",
            "--local",
            f"dockerfile={build_root}",
            "--opt",
            f"filename={dockerfile.name}",
            "--opt",
            "target=export",
            "--output",
            f"type=local,dest={destination.parent}",
        ),
        cwd=root,
    )
    if not destination.is_file():
        raise RuntimeError(f"BuildKit did not export {destination.name}")
    return destination


def _release_output(root: Path) -> Path:
    output = root / "target" / "release-artifacts"
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)
    return output


def _build_cli_artifacts(root: Path, version: str, output: Path) -> None:

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
        built_binary = _builder_build(
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


def _generator_wasm(root: Path, package: str) -> Path:
    wasm_dir = root / "target" / "wasm32-unknown-unknown" / "release"
    return (wasm_dir / package.replace("-", "_")).with_suffix(".wasm")


def _archive_generator(
    root: Path,
    package: str,
    version: str,
    output: Path,
) -> None:
    language = package.removeprefix("csilgen-").removesuffix("-generator")
    wasm = _generator_wasm(root, package)
    archive = output / f"csilgen-generator-{language}-{version}.tar.gz"
    _tar_files(
        archive,
        (
            (wasm, wasm.name),
            (root / "LICENSE", "LICENSE"),
        ),
    )


def _archive_transport(
    root: Path,
    language: str,
    version: str,
    output: Path,
) -> None:
    archive = output / f"csilgen-transport-{language}-{version}.tar.gz"
    _tar_tracked(
        archive,
        root,
        (f"transports/{language}", "LICENSE"),
    )


def _build_all_release_artifacts(
    root: Path,
    versions: Mapping[str, str],
) -> Path:
    output = _release_output(root)
    _build_cli_artifacts(root, versions[CORE_RELEASE], output)
    _build_wasm(root, release=True, packages=GENERATOR_PACKAGES)
    for release, package in GENERATOR_RELEASES.items():
        _archive_generator(root, package, versions[release], output)
    for release, language in TRANSPORT_RELEASES.items():
        _archive_transport(root, language, versions[release], output)
    return output


def _build_tag_artifacts(root: Path, tag: str) -> Path:
    match = RELEASE_TAG.fullmatch(tag)
    if not match:
        raise RuntimeError(f"The release tag is invalid: {tag}")
    package = match.group("package")
    version = match.group("version")
    output = _release_output(root)
    if package == CORE_RELEASE:
        _build_cli_artifacts(root, version, output)
    elif package in GENERATOR_RELEASES:
        generator = GENERATOR_RELEASES[package]
        _build_wasm(root, release=True, packages=(generator,))
        _archive_generator(root, generator, version, output)
    elif package in TRANSPORT_RELEASES:
        _archive_transport(root, TRANSPORT_RELEASES[package], version, output)
    else:
        raise RuntimeError(f"The release tag has an unknown package: {tag}")
    return output


def _go_environment(root: Path) -> Mapping[str, str]:
    home = root / "target" / "reactorcide-home"
    go_path = home / "go"
    go_cache = home / ".cache" / "go-build"
    go_mod_cache = go_path / "pkg" / "mod"
    for directory in (home, go_path, go_cache, go_mod_cache):
        directory.mkdir(parents=True, exist_ok=True)
    return {
        "HOME": str(home),
        "GOPATH": str(go_path),
        "GOCACHE": str(go_cache),
        "GOMODCACHE": str(go_mod_cache),
    }


def _semver_tags_binary(root: Path) -> tuple[Path, Mapping[str, str]]:
    environment = _go_environment(root)
    tool_dir = root / "target" / "reactorcide-tools" / "semver-tags-v0.6.0"
    binary = tool_dir / "semver-tags"
    if not binary.exists():
        tool_dir.mkdir(parents=True, exist_ok=True)
        _run(
            (
                "go",
                "install",
                "github.com/catalystcommunity/semver-tags@v0.6.0",
            ),
            cwd=root,
            env={**environment, "GOBIN": str(tool_dir)},
        )
    return binary, environment


def _semver_tags(root: Path, *, dry_run: bool) -> dict:
    binary, environment = _semver_tags_binary(root)
    args = [str(binary), "run", "--output_json", "--branch", ""]
    if dry_run:
        args.append("--dry_run")
    result = _run(args, cwd=root, env=environment, capture=True)
    start = result.stdout.find("{")
    if start < 0:
        raise RuntimeError("semver-tags did not return JSON output")
    return json.loads(result.stdout[start:])


def _split_metadata(metadata: Mapping[str, Any], key: str) -> List[str]:
    value = metadata.get(key)
    if not isinstance(value, str):
        raise RuntimeError(f"semver-tags did not return {key}")
    return value.split(",")


def _release_plans(metadata: Mapping[str, Any]) -> List[ReleasePlan]:
    packages = _split_metadata(metadata, "Release_package")
    published = _split_metadata(metadata, "New_release_published")
    versions = _split_metadata(metadata, "New_release_version")
    tags = _split_metadata(metadata, "New_release_git_tag")
    source_shas = _split_metadata(metadata, "New_release_git_head")
    fields = (published, versions, tags, source_shas)
    if any(len(field) != len(packages) for field in fields):
        raise RuntimeError("semver-tags returned release fields of different lengths")
    if tuple(packages) != RELEASE_PACKAGES:
        raise RuntimeError(
            "semver-tags release packages do not match .semver-tags.yaml"
        )

    notes_text = metadata.get("New_release_notes_json")
    if not isinstance(notes_text, str):
        raise RuntimeError("semver-tags did not return release notes JSON")
    notes_json = json.loads(notes_text).get("new_release_notes_escaped", {})

    plans = []
    for package, changed, version, tag, source_sha in zip(
        packages,
        published,
        versions,
        tags,
        source_shas,
    ):
        if changed not in {"true", "false"}:
            raise RuntimeError("semver-tags returned an invalid release decision")
        if tag != f"{package}/v{version}" or not RELEASE_TAG.fullmatch(tag):
            raise RuntimeError(f"semver-tags returned an invalid release tag: {tag}")
        package_notes = notes_json.get(f"package_{package}", [])
        if not isinstance(package_notes, list) or not all(
            isinstance(note, str) for note in package_notes
        ):
            raise RuntimeError(f"semver-tags returned invalid notes for {package}")
        plans.append(
            ReleasePlan(
                package=package,
                published=changed == "true",
                version=version,
                tag=tag,
                source_sha=source_sha,
                notes="\n".join(package_notes),
            )
        )
    return plans


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
) -> Any:
    request = urllib.request.Request(url, data=body, method=method)
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Accept", "application/vnd.github+json")
    request.add_header("X-GitHub-Api-Version", "2022-11-28")
    if body is not None:
        request.add_header("Content-Type", content_type)
    with urllib.request.urlopen(request) as response:
        payload = response.read()
    return json.loads(payload) if payload else {}


def _release_marker(source_sha: str) -> str:
    return f"{RELEASE_MARKER_PREFIX}{source_sha} -->"


def _find_github_release(
    token: str,
    repository: str,
    tag: str,
) -> dict | None:
    api = f"https://api.github.com/repos/{repository}"
    encoded_tag = urllib.parse.quote(tag, safe="")
    try:
        published = _github_request(
            "GET",
            f"{api}/releases/tags/{encoded_tag}",
            token,
        )
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
    else:
        if isinstance(published, dict):
            return published

    for page in range(1, 11):
        releases = _github_request(
            "GET",
            f"{api}/releases?per_page=100&page={page}",
            token,
        )
        if not isinstance(releases, list):
            raise RuntimeError("GitHub returned an invalid release list")
        for release in releases:
            if release.get("tag_name") == tag:
                return release
        if len(releases) < 100:
            break
    return None


def _authorized_release(
    token: str,
    repository: str,
    tag: str,
    source_sha: str,
) -> dict:
    release = _find_github_release(token, repository, tag)
    if release is None:
        raise RuntimeError(f"No CI-created draft authorizes {tag}")
    if _release_marker(source_sha) not in str(release.get("body") or ""):
        raise RuntimeError(f"The GitHub Release does not authorize {tag} at HEAD")
    return release


def _create_or_reuse_draft(
    token: str,
    repository: str,
    plan: ReleasePlan,
) -> dict:
    existing = _find_github_release(token, repository, plan.tag)
    if existing is not None:
        if not existing.get("draft"):
            raise RuntimeError(f"A published GitHub Release already uses {plan.tag}")
        if _release_marker(plan.source_sha) not in str(existing.get("body") or ""):
            raise RuntimeError(f"An unrelated draft GitHub Release uses {plan.tag}")
        log_stdout(f"Reuse draft GitHub Release {plan.tag}")
        return existing

    api = f"https://api.github.com/repos/{repository}"
    notes = plan.notes or "No release notes were generated."
    body = f"{notes}\n\n{_release_marker(plan.source_sha)}"
    payload = json.dumps(
        {
            "tag_name": plan.tag,
            "target_commitish": plan.source_sha,
            "name": f"csilgen {plan.tag}",
            "body": body,
            "draft": True,
            "prerelease": False,
        }
    ).encode("utf-8")
    release = _github_request("POST", f"{api}/releases", token, body=payload)
    if not isinstance(release, dict):
        raise RuntimeError("GitHub returned an invalid release")
    log_stdout(f"Created draft GitHub Release {plan.tag}")
    return release


def _upload_release_artifacts(
    token: str,
    repository: str,
    release: Mapping[str, Any],
    artifacts: Path,
) -> None:
    api = f"https://api.github.com/repos/{repository}"

    upload_url_value = release.get("upload_url")
    assets_value = release.get("assets", [])
    if not isinstance(upload_url_value, str) or not isinstance(assets_value, list):
        raise RuntimeError("GitHub returned invalid release upload data")
    upload_url = upload_url_value.split("{", 1)[0]
    existing = {asset["name"]: asset["id"] for asset in assets_value}
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


def _fetch_tags(root: Path, repository: str) -> None:
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


def _package(root: Path) -> None:
    repository = os.environ.get(
        "REACTORCIDE_REPO", "catalystcommunity/csilgen"
    )
    _fetch_tags(root, repository)
    plans = _release_plans(_semver_tags(root, dry_run=True))
    artifacts = _build_all_release_artifacts(
        root,
        {plan.package: plan.version for plan in plans},
    )
    log_stdout(f"Release dry run created artifacts in {artifacts}")


def _tag_releases(root: Path) -> None:
    repository = os.environ.get(
        "REACTORCIDE_REPO", "catalystcommunity/csilgen"
    )
    _fetch_tags(root, repository)
    preview = _release_plans(_semver_tags(root, dry_run=True))
    changed = [plan for plan in preview if plan.published]
    if not changed:
        log_stdout("No release tags are required.")
        return

    token = os.environ.get("GITHUB_PAT")
    if not token:
        raise RuntimeError("GITHUB_PAT is required to tag releases")

    for plan in changed:
        _create_or_reuse_draft(token, repository, plan)

    _configure_git_auth(root, token, repository)
    try:
        result = _release_plans(_semver_tags(root, dry_run=False))
    finally:
        _clear_git_auth(root)
    actual = {plan.tag for plan in result if plan.published}
    expected = {plan.tag for plan in changed}
    if actual != expected:
        raise RuntimeError("semver-tags pushed a different release plan")
    log_stdout(f"Pushed {len(actual)} release tag(s) atomically")


def _release_tag_from_environment(root: Path) -> str:
    tag = os.environ.get("REACTORCIDE_BRANCH", "")
    if tag:
        if not RELEASE_TAG.fullmatch(tag):
            raise RuntimeError(f"REACTORCIDE_BRANCH is not a release tag: {tag}")
        return tag
    tags = _run(
        ("git", "tag", "--points-at", "HEAD"),
        cwd=root,
        capture=True,
    )
    matches = [tag for tag in tags.stdout.splitlines() if RELEASE_TAG.fullmatch(tag)]
    if len(matches) != 1:
        raise RuntimeError("A release job requires one release tag")
    return matches[0]


def _publish_tag_release(root: Path) -> None:
    repository = os.environ.get(
        "REACTORCIDE_REPO", "catalystcommunity/csilgen"
    )
    token = os.environ.get("GITHUB_PAT")
    if not token:
        raise RuntimeError("GITHUB_PAT is required to publish a release")
    tag = _release_tag_from_environment(root)
    source_sha = _run(("git", "rev-parse", "HEAD"), cwd=root, capture=True)
    release = _authorized_release(
        token,
        repository,
        tag,
        source_sha.stdout.strip(),
    )
    artifacts = _build_tag_artifacts(root, tag)
    _upload_release_artifacts(token, repository, release, artifacts)

    api = f"https://api.github.com/repos/{repository}"
    assets = _github_request(
        "GET",
        f"{api}/releases/{release['id']}/assets?per_page=100",
        token,
    )
    if not isinstance(assets, list):
        raise RuntimeError("GitHub returned an invalid release asset list")
    actual_names = {asset["name"] for asset in assets}
    expected_names = {artifact.name for artifact in artifacts.glob("*.tar.gz")}
    missing = sorted(expected_names - actual_names)
    if missing:
        raise RuntimeError("The GitHub Release is missing assets: " + ", ".join(missing))

    if release.get("draft"):
        payload = json.dumps({"draft": False}).encode("utf-8")
        _github_request(
            "PATCH",
            f"{api}/releases/{release['id']}",
            token,
            body=payload,
        )
        log_stdout(f"Published GitHub Release {tag}")
    else:
        log_stdout(f"Updated GitHub Release {tag}")


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
        job = os.environ.get("REACTORCIDE_CSILGEN_JOB")
        if not job:
            raise RuntimeError(
                "REACTORCIDE_CSILGEN_JOB must select a runnerlib lifecycle job"
            )

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
            _package(root)
        elif job == "release-tag":
            _tag_releases(root)
        elif job == "release":
            _publish_tag_release(root)
        else:
            raise RuntimeError(f"Unknown REACTORCIDE_CSILGEN_JOB value: {job}")
