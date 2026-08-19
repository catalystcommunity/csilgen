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
from src.workflow import workflow_vars


ASSET_CACHE_PATH = (
    Path(__file__).resolve().parents[1] / "scripts" / "asset_cache.py"
)
ASSET_CACHE_SPEC = importlib.util.spec_from_file_location(
    "csilgen_asset_cache",
    ASSET_CACHE_PATH,
)
if ASSET_CACHE_SPEC is None or ASSET_CACHE_SPEC.loader is None:
    raise RuntimeError("The CSILgen asset-cache module is not available")
ASSET_CACHE = importlib.util.module_from_spec(ASSET_CACHE_SPEC)
sys.modules[ASSET_CACHE_SPEC.name] = ASSET_CACHE
ASSET_CACHE_SPEC.loader.exec_module(ASSET_CACHE)


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

RELEASE_PACKAGE = "csilgen"
RELEASE_PACKAGES = (RELEASE_PACKAGE,)
SEMVER_TAGS_CONFIG = Path(".reactorcide/semver-tags-release.yaml")
RELEASE_TAG = re.compile(
    r"^csilgen/v(?P<version>"
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$"
)
RELEASE_MARKER_PREFIX = "<!-- csilgen-release-source:"
CLI_PLATFORMS = (
    "linux-x86_64",
    "linux-aarch64",
    "darwin-aarch64",
    "windows-x86_64",
)
GENERATOR_ASSET = "generators.tar.gz"
TRANSPORT_ASSETS = tuple(f"transport-{language}.tar.gz" for language in TRANSPORTS)
EXPECTED_CACHE_ASSETS = (
    *(f"cli-{platform}.tar.gz" for platform in CLI_PLATFORMS),
    GENERATOR_ASSET,
    *TRANSPORT_ASSETS,
)


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


def _cli_builds() -> Mapping[str, tuple[str, str, tuple[str, ...], str, str]]:
    zig_image = "ghcr.io/rust-cross/cargo-zigbuild:latest"
    return {
        platform: (image, target, command, binary, archive_name)
        for platform, image, target, command, binary, archive_name in (
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
    }


def _build_cli_asset(
    root: Path,
    platform_name: str,
    output: Path,
    archive_name: str,
) -> Path:
    build = _cli_builds().get(platform_name)
    if build is None:
        raise RuntimeError(f"The CLI release platform is invalid: {platform_name}")
    image, target, command, binary, binary_name = build
    built_binary = _builder_build(root, image, target, command, binary)
    archive = output / archive_name
    _tar_files(
        archive,
        (
            (built_binary, binary_name),
            (root / "LICENSE", "LICENSE"),
            (root / "README.md", "README.md"),
        ),
    )
    return archive


def _build_cli_artifacts(root: Path, version: str, output: Path) -> None:
    for platform_name in CLI_PLATFORMS:
        _build_cli_asset(
            root,
            platform_name,
            output,
            f"csilgen-{version}-{platform_name}.tar.gz",
        )


def _generator_wasm(root: Path, package: str) -> Path:
    wasm_dir = root / "target" / "wasm32-unknown-unknown" / "release"
    return (wasm_dir / package.replace("-", "_")).with_suffix(".wasm")


def _archive_generators(root: Path, output: Path, archive_name: str) -> Path:
    files = [
        (_generator_wasm(root, package), _generator_wasm(root, package).name)
        for package in GENERATOR_PACKAGES
    ]
    files.append((root / "LICENSE", "LICENSE"))
    archive = output / archive_name
    _tar_files(archive, files)
    return archive


def _archive_transport(
    root: Path,
    language: str,
    output: Path,
    archive_name: str,
) -> Path:
    archive = output / archive_name
    _tar_tracked(
        archive,
        root,
        (f"transports/{language}", "LICENSE"),
    )
    return archive


def _release_asset_name(asset: str, version: str) -> str:
    if asset.startswith("cli-"):
        platform_name = asset.removeprefix("cli-").removesuffix(".tar.gz")
        return f"csilgen-{version}-{platform_name}.tar.gz"
    if asset == GENERATOR_ASSET:
        return f"csilgen-generators-{version}.tar.gz"
    if asset.startswith("transport-"):
        language = asset.removeprefix("transport-").removesuffix(".tar.gz")
        return f"csilgen-transport-{language}-{version}.tar.gz"
    raise RuntimeError(f"The release asset is invalid: {asset}")


def _build_cache_asset(root: Path, asset: str) -> Path:
    output = _release_output(root)
    if asset.startswith("cli-"):
        platform_name = asset.removeprefix("cli-").removesuffix(".tar.gz")
        return _build_cli_asset(root, platform_name, output, asset)
    if asset == GENERATOR_ASSET:
        _build_wasm(root, release=True, packages=GENERATOR_PACKAGES)
        return _archive_generators(root, output, asset)
    if asset == "transports.tar.gz":
        raise RuntimeError("Transport assets must remain separate archives")
    if asset.startswith("transport-"):
        language = asset.removeprefix("transport-").removesuffix(".tar.gz")
        if language not in TRANSPORTS:
            raise RuntimeError(f"The transport release asset is invalid: {asset}")
        return _archive_transport(root, language, output, asset)
    raise RuntimeError(f"The cache asset is invalid: {asset}")


def _build_all_release_artifacts(
    root: Path,
    versions: Mapping[str, str],
) -> Path:
    output = _release_output(root)
    version = versions[RELEASE_PACKAGE]
    _build_cli_artifacts(root, version, output)
    _build_wasm(root, release=True, packages=GENERATOR_PACKAGES)
    _archive_generators(root, output, f"csilgen-generators-{version}.tar.gz")
    for language in TRANSPORTS:
        _archive_transport(
            root,
            language,
            output,
            f"csilgen-transport-{language}-{version}.tar.gz",
        )
    return output


def _build_tag_artifacts(root: Path, tag: str) -> Path:
    match = RELEASE_TAG.fullmatch(tag)
    if not match:
        raise RuntimeError(f"The release tag is invalid: {tag}")
    version = match.group("version")
    return _build_all_release_artifacts(root, {RELEASE_PACKAGE: version})


def _workflow_vars() -> Mapping[str, Any]:
    return workflow_vars()


def _set_workflow_vars(values: Mapping[str, Any]) -> None:
    path = os.environ.get("RC_WF_OUTPUT_FILE", "")
    if not path:
        raise RuntimeError("RC_WF_OUTPUT_FILE is required for asset jobs")
    output = Path(path)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps({"vars": dict(values), "outputs": {}}, sort_keys=True),
        encoding="utf-8",
    )


def _git_tree(root: Path, revision: str = "HEAD") -> str:
    result = _run(("git", "rev-parse", f"{revision}^{{tree}}"), cwd=root, capture=True)
    tree = result.stdout.strip()
    if not re.fullmatch(r"[0-9a-f]{40,64}", tree):
        raise RuntimeError("Git returned an invalid tree hash")
    return tree


def _asset_for_job() -> str:
    kind = os.environ.get("CSILGEN_ASSET_KIND", "")
    item = os.environ.get("CSILGEN_ASSET_ITEM", "")
    if kind == "cli" and item in CLI_PLATFORMS:
        return f"cli-{item}.tar.gz"
    if kind == "generators" and not item:
        return GENERATOR_ASSET
    if kind == "transport" and item in TRANSPORTS:
        return f"transport-{item}.tar.gz"
    raise RuntimeError("The release asset job selection is invalid")


def _prepare_asset_lane(root: Path) -> None:
    cache = ASSET_CACHE.S3Cache.from_environment()
    event = os.environ.get("REACTORCIDE_EVENT_TYPE", "")
    if event in {"pull_request_opened", "pull_request_updated"}:
        source_sha = os.environ.get("REACTORCIDE_SHA", "")
        lane = ASSET_CACHE.pr_lane(
            os.environ.get("REACTORCIDE_PR_NUMBER", ""),
            source_sha,
        )
    elif event == "tag_created":
        tag = _release_tag_from_environment(root)
        match = RELEASE_TAG.fullmatch(tag)
        if match is None:
            raise RuntimeError("The release tag is invalid")
        lane = ASSET_CACHE.version_lane(match.group("version"))
        source_sha = _run(("git", "rev-parse", "HEAD"), cwd=root, capture=True).stdout.strip()
        token = os.environ.get("GITHUB_PAT", "")
        if not token:
            raise RuntimeError("GITHUB_PAT is required to prepare a release")
        _authorized_release(
            token,
            os.environ.get("REACTORCIDE_REPO", "catalystcommunity/csilgen"),
            tag,
            source_sha,
        )
    else:
        raise RuntimeError("Asset preparation requires a PR or tag event")

    cached_assets: set[str] = set()
    try:
        manifest = _read_lane_manifest(cache, lane, verify_files=True)
    except (FileNotFoundError, RuntimeError, ValueError, json.JSONDecodeError):
        log_stdout(f"The existing lane {lane} is not reusable; rebuild its assets")
        manifest = None
    if manifest is not None:
        if manifest.get("source_sha") == source_sha and manifest.get("source_tree") == _git_tree(root):
            cached_assets = set(EXPECTED_CACHE_ASSETS)

    uploads: dict[str, dict[str, str]] = {}
    for asset in EXPECTED_CACHE_ASSETS:
        if asset in cached_assets:
            continue
        staging_asset = "staging-" + asset
        uploads[asset] = {
            "asset": cache.presign(
                "PUT", ASSET_CACHE.object_key(lane, staging_asset)
            ),
            "sha256": cache.presign(
                "PUT",
                ASSET_CACHE.object_key(lane, staging_asset + ".sha256"),
            ),
        }
    _set_workflow_vars(
        {
            "asset_cache_lane": lane,
            "asset_cache_uploads": uploads,
            "asset_cache_source_sha": source_sha,
            "asset_cache_source_tree": _git_tree(root),
        }
    )
    log_stdout(
        f"Prepared asset lane {lane} with {len(uploads)} build upload set(s)"
    )


def _put_presigned(url: str, content: bytes) -> None:
    request = urllib.request.Request(url, data=content, method="PUT")
    for attempt in range(5):
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                response.read()
            return
        except urllib.error.HTTPError as error:
            if error.code not in {408, 429, 500, 502, 503, 504} or attempt == 4:
                raise RuntimeError(
                    f"The exact-object asset upload failed with HTTP {error.code}"
                ) from None
        except (urllib.error.URLError, TimeoutError):
            if attempt == 4:
                raise RuntimeError("The exact-object asset upload failed") from None
        time.sleep(2**attempt)
    raise RuntimeError("The exact-object asset upload failed")


def _build_and_upload_asset(root: Path) -> None:
    asset = _asset_for_job()
    variables = _workflow_vars()
    uploads = variables.get("asset_cache_uploads")
    if not isinstance(uploads, dict):
        raise RuntimeError("The asset upload map is missing")
    upload = uploads.get(asset)
    if upload is None:
        log_stdout(f"Reuse sealed cache asset {asset}")
        return
    if not isinstance(upload, dict):
        raise RuntimeError("The asset upload entry is invalid")
    asset_url = upload.get("asset")
    digest_url = upload.get("sha256")
    if not isinstance(asset_url, str) or not isinstance(digest_url, str):
        raise RuntimeError("The asset upload URLs are invalid")
    archive = _build_cache_asset(root, asset)
    digest = ASSET_CACHE.file_sha256(archive)
    _put_presigned(asset_url, archive.read_bytes())
    _put_presigned(digest_url, (digest + "\n").encode())
    log_stdout(f"Built and uploaded cache asset {asset}")


def _read_lane_manifest(
    cache: Any,
    lane: str,
    *,
    verify_files: bool,
) -> dict[str, Any]:
    content = cache.get_bytes(ASSET_CACHE.object_key(lane, ASSET_CACHE.MANIFEST))
    manifest = ASSET_CACHE.decode_manifest(content)
    assets = manifest.get("assets")
    if not isinstance(assets, list) or len(assets) != len(EXPECTED_CACHE_ASSETS):
        raise RuntimeError("The asset-cache manifest has the wrong asset count")
    by_name = {item.get("name"): item for item in assets if isinstance(item, dict)}
    if set(by_name) != set(EXPECTED_CACHE_ASSETS):
        raise RuntimeError("The asset-cache manifest has unexpected assets")
    if verify_files:
        for name in EXPECTED_CACHE_ASSETS:
            item = by_name[name]
            content = cache.get_bytes(ASSET_CACHE.object_key(lane, name))
            actual = hashlib.sha256(content).hexdigest()
            if item.get("sha256") != actual or item.get("size") != len(content):
                raise RuntimeError(f"The cached asset is invalid: {name}")
    return manifest


def _seal_asset_lane(root: Path) -> None:
    variables = _workflow_vars()
    lane = variables.get("asset_cache_lane")
    source_sha = variables.get("asset_cache_source_sha")
    source_tree = variables.get("asset_cache_source_tree")
    if not all(isinstance(value, str) for value in (lane, source_sha, source_tree)):
        raise RuntimeError("The asset lane variables are invalid")
    cache = ASSET_CACHE.S3Cache.from_environment()
    uploads = variables.get("asset_cache_uploads")
    if not isinstance(uploads, dict):
        raise RuntimeError("The asset upload map is missing")
    assets = []
    for asset in EXPECTED_CACHE_ASSETS:
        if asset in uploads:
            staging_asset = "staging-" + asset
            staging_key = ASSET_CACHE.object_key(lane, staging_asset)
            staging_digest_key = ASSET_CACHE.object_key(
                lane, staging_asset + ".sha256"
            )
            content = cache.get_bytes(staging_key)
            recorded = cache.get_bytes(staging_digest_key).decode().strip()
        else:
            content = cache.get_bytes(ASSET_CACHE.object_key(lane, asset))
            recorded = cache.get_bytes(
                ASSET_CACHE.object_key(lane, asset + ".sha256")
            ).decode().strip()
        digest = hashlib.sha256(content).hexdigest()
        if recorded != digest:
            raise RuntimeError(f"The asset checksum does not match: {asset}")
        if asset in uploads:
            final_key = ASSET_CACHE.object_key(lane, asset)
            cache.copy(staging_key, final_key)
            copied = cache.get_bytes(final_key)
            if hashlib.sha256(copied).hexdigest() != digest or len(copied) != len(content):
                raise RuntimeError(f"The sealed asset copy is invalid: {asset}")
            cache.put_bytes(
                ASSET_CACHE.object_key(lane, asset + ".sha256"),
                (digest + "\n").encode(),
            )
            cache.delete(staging_key)
            cache.delete(staging_digest_key)
        assets.append({"name": asset, "sha256": digest, "size": len(content)})
    manifest = {
        "schema": 1,
        "project": ASSET_CACHE.PROJECT,
        "lane": lane,
        "source_sha": source_sha,
        "source_tree": source_tree,
        "created_at": time.time(),
        "assets": assets,
    }
    cache.put_bytes(
        ASSET_CACHE.object_key(lane, ASSET_CACHE.MANIFEST),
        ASSET_CACHE.encode_manifest(manifest),
    )
    log_stdout(f"Sealed asset lane {lane}")


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
    args = [
        str(binary),
        "--config",
        str(root / SEMVER_TAGS_CONFIG),
        "run",
        "--output_json",
        "--branch",
        "",
    ]
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

    if len(changed) != 1:
        raise RuntimeError("The unified release must create exactly one tag")
    _promote_merged_pr_assets(root, token, repository, changed[0])

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


def _promote_merged_pr_assets(
    root: Path,
    token: str,
    repository: str,
    plan: ReleasePlan,
) -> bool:
    pr_number = os.environ.get("REACTORCIDE_PR_NUMBER", "")
    if not pr_number.isdigit():
        log_stdout("No PR number is available; the tag workflow will rebuild assets")
        return False
    api = f"https://api.github.com/repos/{repository}"
    pull = _github_request("GET", f"{api}/pulls/{pr_number}", token)
    if not isinstance(pull, dict) or pull.get("merged") is not True:
        raise RuntimeError("GitHub did not return a merged pull request")
    base = pull.get("base")
    base_repo = base.get("repo") if isinstance(base, dict) else None
    if (
        not isinstance(base, dict)
        or base.get("ref") != "main"
        or not isinstance(base_repo, dict)
        or base_repo.get("full_name") != repository
    ):
        raise RuntimeError("The merged pull request has an unexpected base")
    if pull.get("merge_commit_sha") != plan.source_sha:
        raise RuntimeError("The merged pull request does not match the release commit")
    head = pull.get("head") if isinstance(pull, dict) else None
    head_sha = head.get("sha") if isinstance(head, dict) else None
    head_repo = head.get("repo") if isinstance(head, dict) else None
    head_repository = head_repo.get("full_name") if isinstance(head_repo, dict) else None
    if (
        not isinstance(head_sha, str)
        or not re.fullmatch(r"[0-9a-fA-F]{40,64}", head_sha)
        or not isinstance(head_repository, str)
        or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", head_repository)
    ):
        log_stdout("The PR head metadata is unavailable; the tag workflow will build assets")
        return False
    source_lane = ASSET_CACHE.pr_lane(pr_number, head_sha)
    cache = ASSET_CACHE.S3Cache.from_environment()
    try:
        manifest = _read_lane_manifest(cache, source_lane, verify_files=True)
    except (FileNotFoundError, RuntimeError, ValueError, json.JSONDecodeError):
        log_stdout(
            f"PR lane {source_lane} is not reusable; the tag workflow will build assets"
        )
        return False
    if manifest.get("source_sha") != head_sha:
        log_stdout("The PR cache commit is stale; the tag workflow will build assets")
        return False
    head_api = f"https://api.github.com/repos/{head_repository}"
    try:
        head_commit = _github_request(
            "GET", f"{head_api}/git/commits/{head_sha}", token
        )
    except RuntimeError:
        log_stdout("The PR head tree is unavailable; the tag workflow will build assets")
        return False
    tree = head_commit.get("tree") if isinstance(head_commit, dict) else None
    head_tree = tree.get("sha") if isinstance(tree, dict) else None
    merge_tree = _git_tree(root)
    if manifest.get("source_tree") != head_tree:
        log_stdout("The PR cache tree is stale; the tag workflow will build assets")
        return False
    if head_tree != merge_tree:
        log_stdout(
            "The merged tree differs from the tested PR tree; "
            "the tag workflow will rebuild assets"
        )
        return False

    version_match = RELEASE_TAG.fullmatch(plan.tag)
    if version_match is None:
        raise RuntimeError("The calculated release tag is invalid")
    destination_lane = ASSET_CACHE.version_lane(version_match.group("version"))
    merged_lane = ASSET_CACHE.main_lane(plan.source_sha)
    for lane in (destination_lane, merged_lane):
        for asset in EXPECTED_CACHE_ASSETS:
            source_content = cache.get_bytes(ASSET_CACHE.object_key(source_lane, asset))
            cache.copy(
                ASSET_CACHE.object_key(source_lane, asset),
                ASSET_CACHE.object_key(lane, asset),
            )
            destination_content = cache.get_bytes(ASSET_CACHE.object_key(lane, asset))
            if (
                hashlib.sha256(destination_content).digest()
                != hashlib.sha256(source_content).digest()
                or len(destination_content) != len(source_content)
            ):
                raise RuntimeError(f"The promoted asset copy is invalid: {asset}")
            cache.put_bytes(
                ASSET_CACHE.object_key(lane, asset + ".sha256"),
                (hashlib.sha256(destination_content).hexdigest() + "\n").encode(),
            )
        promoted = dict(manifest)
        promoted.update(
            {
                "lane": lane,
                "source_sha": plan.source_sha,
                "source_tree": merge_tree,
                "created_at": time.time(),
                "promoted_from": source_lane,
                "main_lane": merged_lane,
            }
        )
        cache.put_bytes(
            ASSET_CACHE.object_key(lane, ASSET_CACHE.MANIFEST),
            ASSET_CACHE.encode_manifest(promoted),
        )
    log_stdout(f"Promoted sealed PR assets from {source_lane} to {destination_lane}")
    return True


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
    match = RELEASE_TAG.fullmatch(tag)
    if match is None:
        raise RuntimeError("The release tag is invalid")
    version = match.group("version")
    lane = ASSET_CACHE.version_lane(version)
    cache = ASSET_CACHE.S3Cache.from_environment()
    manifest = _read_lane_manifest(cache, lane, verify_files=True)
    if manifest.get("source_sha") != source_sha.stdout.strip():
        raise RuntimeError("The release cache has the wrong source commit")
    if manifest.get("source_tree") != _git_tree(root):
        raise RuntimeError("The release cache has the wrong source tree")
    artifacts = _release_output(root)
    for asset in EXPECTED_CACHE_ASSETS:
        cache.get_file(
            ASSET_CACHE.object_key(lane, asset),
            artifacts / _release_asset_name(asset, version),
        )
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

    merged_lane = ASSET_CACHE.main_lane(source_sha.stdout.strip())
    for asset in EXPECTED_CACHE_ASSETS:
        source_content = cache.get_bytes(ASSET_CACHE.object_key(lane, asset))
        cache.copy(
            ASSET_CACHE.object_key(lane, asset),
            ASSET_CACHE.object_key(merged_lane, asset),
        )
        destination_content = cache.get_bytes(
            ASSET_CACHE.object_key(merged_lane, asset)
        )
        if (
            hashlib.sha256(destination_content).digest()
            != hashlib.sha256(source_content).digest()
            or len(destination_content) != len(source_content)
        ):
            raise RuntimeError(f"The main-lane asset copy is invalid: {asset}")
        cache.put_bytes(
            ASSET_CACHE.object_key(merged_lane, asset + ".sha256"),
            (hashlib.sha256(destination_content).hexdigest() + "\n").encode(),
        )
    main_manifest = dict(manifest)
    main_manifest.update(
        {
            "lane": merged_lane,
            "created_at": time.time(),
            "release_lane": lane,
            "main_lane": merged_lane,
        }
    )
    cache.put_bytes(
        ASSET_CACHE.object_key(merged_lane, ASSET_CACHE.MANIFEST),
        ASSET_CACHE.encode_manifest(main_manifest),
    )
    version_manifest = dict(manifest)
    version_manifest.update({"main_lane": merged_lane})
    cache.put_bytes(
        ASSET_CACHE.object_key(lane, ASSET_CACHE.MANIFEST),
        ASSET_CACHE.encode_manifest(version_manifest),
    )
    cache.put_bytes(
        ASSET_CACHE.object_key("main", "latest.json"),
        ASSET_CACHE.encode_manifest(
            {
                "schema": 1,
                "project": ASSET_CACHE.PROJECT,
                "lane": "main",
                "main_lane": merged_lane,
                "release_lane": lane,
                "source_sha": source_sha.stdout.strip(),
                "source_tree": _git_tree(root),
                "created_at": time.time(),
            }
        ),
    )
    log_stdout(f"Updated the main asset pointer to {merged_lane}")


def _cleanup_asset_cache() -> None:
    cache = ASSET_CACHE.S3Cache.from_environment()
    prefix = ASSET_CACHE.PROJECT + "/"
    objects = cache.list(prefix)
    by_lane: dict[str, list[Any]] = {}
    for item in objects:
        relative = item.key.removeprefix(prefix)
        lane, separator, _ = relative.partition("/")
        if separator and ASSET_CACHE.LANE_RE.fullmatch(lane):
            by_lane.setdefault(lane, []).append(item)
    version_lanes = sorted(
        (lane for lane in by_lane if ASSET_CACHE.VERSION_LANE_RE.fullmatch(lane)),
        key=ASSET_CACHE.version_sort_key,
        reverse=True,
    )
    completed_versions: list[tuple[str, dict[str, Any], float]] = []
    for lane in version_lanes:
        try:
            manifest = _read_lane_manifest(cache, lane, verify_files=False)
        except (FileNotFoundError, RuntimeError, ValueError, json.JSONDecodeError):
            continue
        created_at = manifest.get("created_at")
        if isinstance(created_at, (int, float)):
            completed_versions.append((lane, manifest, float(created_at)))
        if len(completed_versions) == 6:
            break
    if not completed_versions:
        log_stdout("The asset cache has no complete release lane")
        return
    retained_versions = {lane for lane, _, _ in completed_versions}
    retained_lanes = {"main", *retained_versions}
    retained_times = [created_at for _, _, created_at in completed_versions]
    for _, manifest, _ in completed_versions:
        for field in ("promoted_from", "main_lane"):
            value = manifest.get(field)
            if isinstance(value, str) and ASSET_CACHE.LANE_RE.fullmatch(value):
                retained_lanes.add(value)
        source_sha = manifest.get("source_sha")
        if isinstance(source_sha, str):
            try:
                retained_lanes.add(ASSET_CACHE.main_lane(source_sha))
            except RuntimeError:
                pass
    cutoff = min(retained_times)
    deleted_lanes = 0
    for lane, lane_objects in sorted(by_lane.items()):
        if lane in retained_lanes:
            continue
        try:
            manifest = _read_lane_manifest(cache, lane, verify_files=False)
        except (FileNotFoundError, RuntimeError, ValueError, json.JSONDecodeError):
            lane_time = max(item.last_modified.timestamp() for item in lane_objects)
        else:
            created_at = manifest.get("created_at")
            object_time = max(
                item.last_modified.timestamp() for item in lane_objects
            )
            lane_time = (
                max(float(created_at), object_time)
                if isinstance(created_at, (int, float))
                else object_time
            )
        if lane_time >= cutoff:
            continue
        for item in lane_objects:
            if not item.key.startswith(prefix + lane + "/"):
                raise RuntimeError("The cleanup object escaped its validated lane")
            cache.delete(item.key)
        deleted_lanes += 1
    log_stdout(f"Deleted {deleted_lanes} expired asset-cache lane(s)")


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
            raise RuntimeError(
                "CSILGEN_JOB must select a runnerlib lifecycle job"
            )

        if job == "conventional-commits":
            _validate_conventional_commits(root)
        elif job == "core":
            _test_core(root)
        elif job == "generators":
            _test_generators(root)
        elif job == "transports":
            _test_transports(root)
        elif job == "interop":
            _test_interop(root)
        elif job == "package":
            _package(root)
        elif job == "asset-prepare":
            _prepare_asset_lane(root)
        elif job == "asset-build":
            _build_and_upload_asset(root)
        elif job == "asset-seal":
            _seal_asset_lane(root)
        elif job == "asset-cleanup":
            _cleanup_asset_cache()
        elif job == "release-tag":
            _tag_releases(root)
        elif job == "release":
            _publish_tag_release(root)
        else:
            raise RuntimeError(f"Unknown CSILGEN_JOB value: {job}")
