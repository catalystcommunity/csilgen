#!/usr/bin/env bash

set -euo pipefail

CSILGEN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CSILGEN_CI_DOCKERFILE="$CSILGEN_ROOT/tools/ci-image/Dockerfile"
CSILGEN_CI_IMAGE="containers.catalystsquad.com/private/catalystcommunity/csilgen/ci-toolchain"
CSILGEN_REGISTRY="containers.catalystsquad.com"
CSILGEN_GITHUB_API="${CSILGEN_GITHUB_API:-https://api.github.com/repos/catalystcommunity/csilgen/releases?per_page=100}"
CSILGEN_DOCKER_CONFIG=""
CSILGEN_INSTALL_TEMP=""
CSILGEN_GENERATOR_PACKAGES=(
    csilgen-c-generator
    csilgen-csharp-generator
    csilgen-dart-generator
    csilgen-elixir-generator
    csilgen-go-generator
    csilgen-java-generator
    csilgen-json-generator
    csilgen-kotlin-generator
    csilgen-ocaml-generator
    csilgen-openapi-generator
    csilgen-php-generator
    csilgen-python-generator
    csilgen-ruby-generator
    csilgen-rust-generator
    csilgen-swift-generator
    csilgen-typescript-generator
    csilgen-zig-generator
)

usage() {
    printf '%s\n' \
        "Usage: ./tools.sh build-install-all" \
        "       ./tools.sh install-all" \
        "       ./tools.sh build-ci-image [TAG]" \
        "       ./tools.sh publish-ci-image [TAG]" \
        "" \
        "build-install-all builds and installs the CLI and production generators." \
        "install-all installs the latest GitHub Release for this system." \
        "build-ci-image builds the CSILgen CI toolchain for linux/amd64." \
        "publish-ci-image builds and publishes TAG and latest." \
        "The default image TAG is derived from the Dockerfile content."
}

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'Required command is not available: %s\n' "$1" >&2
        exit 1
    fi
}

install_root() {
    if [[ -n "${CARGO_HOME:-}" ]]; then
        printf '%s\n' "$CARGO_HOME"
    else
        printf '%s\n' "$HOME/.cargo"
    fi
}

generator_install_dir() {
    if [[ -n "${CSILGEN_GENERATOR_DIR:-}" ]]; then
        printf '%s\n' "$CSILGEN_GENERATOR_DIR"
    else
        printf '%s\n' "$HOME/.csilgen/generators"
    fi
}

generator_wasm_name() {
    local package_name="$1"
    printf '%s.wasm\n' "${package_name//-/_}"
}

verify_installation() {
    local binary_path="$1"
    local generator_dir="$2"
    local package_name
    local wasm_name

    "$binary_path" --version
    for package_name in "${CSILGEN_GENERATOR_PACKAGES[@]}"; do
        wasm_name="$(generator_wasm_name "$package_name")"
        if [[ ! -s "$generator_dir/$wasm_name" ]]; then
            printf 'The installed generator is missing: %s\n' "$generator_dir/$wasm_name" >&2
            exit 1
        fi
    done
    printf 'Installed CLI: %s\n' "$binary_path"
    printf 'Installed %d generators: %s\n' \
        "${#CSILGEN_GENERATOR_PACKAGES[@]}" "$generator_dir"
}

build_install_all() {
    local cargo_root
    local binary_path
    local generator_dir
    local package_name
    local source_path
    local wasm_name
    local -a cargo_args

    require_command cargo
    require_command install
    require_command rustup

    cargo_root="$(install_root)"
    binary_path="$cargo_root/bin/csilgen"
    generator_dir="$(generator_install_dir)"

    rustup target add wasm32-unknown-unknown
    cargo install \
        --locked \
        --force \
        --root "$cargo_root" \
        --path "$CSILGEN_ROOT/crates/csilgen-cli"

    cargo_args=(
        build
        --release
        --target wasm32-unknown-unknown
        --target-dir "$CSILGEN_ROOT/target"
        --manifest-path "$CSILGEN_ROOT/Cargo.toml"
    )
    for package_name in "${CSILGEN_GENERATOR_PACKAGES[@]}"; do
        cargo_args+=(--package "$package_name")
    done
    cargo "${cargo_args[@]}"

    mkdir -p "$generator_dir"
    for package_name in "${CSILGEN_GENERATOR_PACKAGES[@]}"; do
        wasm_name="$(generator_wasm_name "$package_name")"
        source_path="$CSILGEN_ROOT/target/wasm32-unknown-unknown/release/$wasm_name"
        install -m 0644 "$source_path" "$generator_dir/$wasm_name"
    done

    verify_installation "$binary_path" "$generator_dir"
}

current_release_platform() {
    local system
    local machine

    system="$(uname -s)"
    machine="$(uname -m)"
    case "$system:$machine" in
        Linux:x86_64|Linux:amd64)
            printf '%s\n' "linux-x86_64"
            ;;
        Linux:aarch64|Linux:arm64)
            printf '%s\n' "linux-aarch64"
            ;;
        Darwin:arm64|Darwin:aarch64)
            printf '%s\n' "darwin-aarch64"
            ;;
        MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64)
            printf '%s\n' "windows-x86_64"
            ;;
        *)
            printf 'No CSILgen release is available for %s on %s.\n' \
                "$machine" "$system" >&2
            exit 1
            ;;
    esac
}

release_asset_url() {
    local release_file="$1"
    local asset_name="$2"
    python3 -c \
        'import json, sys
releases = json.load(open(sys.argv[1], encoding="utf-8"))
release = next((item for item in releases if not item.get("draft") and not item.get("prerelease") and item.get("tag_name", "").startswith("csilgen/v")), None)
if release is None:
    raise SystemExit("No published csilgen/v* release is available")
matches = [asset["browser_download_url"] for asset in release.get("assets", []) if asset.get("name") == sys.argv[2]]
if len(matches) != 1:
    raise SystemExit(f"The release must contain one {sys.argv[2]} asset")
print(matches[0])' \
        "$release_file" "$asset_name"
}

latest_release_tag() {
    local release_file="$1"
    python3 -c \
        'import json, sys
releases = json.load(open(sys.argv[1], encoding="utf-8"))
release = next((item for item in releases if not item.get("draft") and not item.get("prerelease") and item.get("tag_name", "").startswith("csilgen/v")), None)
if release is None:
    raise SystemExit("No published csilgen/v* release is available")
print(release["tag_name"])' \
        "$release_file"
}

cleanup_install_temp() {
    if [[ -n "$CSILGEN_INSTALL_TEMP" && -d "$CSILGEN_INSTALL_TEMP" ]]; then
        find "$CSILGEN_INSTALL_TEMP" -depth -delete
    fi
    CSILGEN_INSTALL_TEMP=""
}

install_all() {
    local platform
    local binary_name="csilgen"
    local cargo_root
    local binary_path
    local generator_dir
    local release_file
    local release_tag
    local version
    local cli_asset
    local generator_asset
    local cli_url
    local generator_url
    local package_name
    local wasm_name
    local -a generator_members

    require_command curl
    require_command install
    require_command python3
    require_command tar

    platform="$(current_release_platform)"
    if [[ "$platform" == "windows-x86_64" ]]; then
        binary_name="csilgen.exe"
    fi
    cargo_root="$(install_root)"
    binary_path="$cargo_root/bin/$binary_name"
    generator_dir="$(generator_install_dir)"

    CSILGEN_INSTALL_TEMP="$(mktemp -d /tmp/csilgen-install.XXXXXX)"
    trap cleanup_install_temp EXIT
    release_file="$CSILGEN_INSTALL_TEMP/release.json"
    curl --fail --location --silent --show-error \
        --retry 3 --retry-all-errors \
        --output "$release_file" "$CSILGEN_GITHUB_API"

    release_tag="$(latest_release_tag "$release_file")"
    if [[ ! "$release_tag" =~ ^csilgen/v([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?)$ ]]; then
        printf 'The latest release tag is not a CSILgen release: %s\n' "$release_tag" >&2
        exit 1
    fi
    version="${BASH_REMATCH[1]}"
    cli_asset="csilgen-$version-$platform.tar.gz"
    generator_asset="csilgen-generators-$version.tar.gz"
    cli_url="$(release_asset_url "$release_file" "$cli_asset")"
    generator_url="$(release_asset_url "$release_file" "$generator_asset")"

    curl --fail --location --silent --show-error \
        --retry 3 --retry-all-errors \
        --output "$CSILGEN_INSTALL_TEMP/$cli_asset" "$cli_url"
    curl --fail --location --silent --show-error \
        --retry 3 --retry-all-errors \
        --output "$CSILGEN_INSTALL_TEMP/$generator_asset" "$generator_url"

    mkdir -p "$CSILGEN_INSTALL_TEMP/cli" "$CSILGEN_INSTALL_TEMP/generators"
    tar -xzf "$CSILGEN_INSTALL_TEMP/$cli_asset" \
        -C "$CSILGEN_INSTALL_TEMP/cli" "$binary_name"
    for package_name in "${CSILGEN_GENERATOR_PACKAGES[@]}"; do
        wasm_name="$(generator_wasm_name "$package_name")"
        generator_members+=("$wasm_name")
    done
    tar -xzf "$CSILGEN_INSTALL_TEMP/$generator_asset" \
        -C "$CSILGEN_INSTALL_TEMP/generators" "${generator_members[@]}"

    mkdir -p "$cargo_root/bin" "$generator_dir"
    install -m 0755 "$CSILGEN_INSTALL_TEMP/cli/$binary_name" "$binary_path"
    for wasm_name in "${generator_members[@]}"; do
        install -m 0644 \
            "$CSILGEN_INSTALL_TEMP/generators/$wasm_name" \
            "$generator_dir/$wasm_name"
    done

    verify_installation "$binary_path" "$generator_dir"
    printf 'Installed GitHub Release: %s\n' "$release_tag"
}

default_ci_image_tag() {
    local digest
    if command -v sha256sum >/dev/null 2>&1; then
        digest="$(sha256sum "$CSILGEN_CI_DOCKERFILE")"
    elif command -v shasum >/dev/null 2>&1; then
        digest="$(shasum -a 256 "$CSILGEN_CI_DOCKERFILE")"
    else
        printf '%s\n' "A SHA-256 command is required." >&2
        exit 1
    fi
    printf 'dockerfile-%s\n' "${digest%% *}" | cut -c 1-27
}

validate_tag() {
    if [[ ! "$1" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
        printf 'The container tag is invalid: %s\n' "$1" >&2
        exit 1
    fi
}

build_ci_image() {
    local tag="$1"
    require_command docker
    validate_tag "$tag"
    if [[ "$(uname -m)" != "x86_64" ]]; then
        printf '%s\n' "The current CI image supports only linux/amd64." >&2
        exit 1
    fi

    printf 'Build %s:%s\n' "$CSILGEN_CI_IMAGE" "$tag"
    docker build \
        --pull \
        --platform linux/amd64 \
        --file "$CSILGEN_CI_DOCKERFILE" \
        --tag "$CSILGEN_CI_IMAGE:$tag" \
        --tag "$CSILGEN_CI_IMAGE:latest" \
        "$CSILGEN_ROOT/tools/ci-image"
}

reactorcide_command() {
    if [[ -n "${CSILGEN_REACTORCIDE_BIN:-}" ]]; then
        printf '%s\n' "$CSILGEN_REACTORCIDE_BIN"
    elif [[ -x "$CSILGEN_ROOT/../reactorcide/coordinator_api/reactorcide" ]]; then
        printf '%s\n' "$CSILGEN_ROOT/../reactorcide/coordinator_api/reactorcide"
    else
        command -v reactorcide
    fi
}

load_registry_credentials() {
    local reactorcide_bin
    if [[ -n "${CSILGEN_REGISTRY_USER:-}" && -n "${CSILGEN_REGISTRY_PASSWORD:-}" ]]; then
        return
    fi

    reactorcide_bin="$(reactorcide_command)"
    if [[ -z "${REACTORCIDE_SECRETS_PASSWORD:-}" && -r "$HOME/.reactorcide-pass" ]]; then
        REACTORCIDE_SECRETS_PASSWORD="$(< "$HOME/.reactorcide-pass")"
        export REACTORCIDE_SECRETS_PASSWORD
    fi
    CSILGEN_REGISTRY_USER="$(
        "$reactorcide_bin" secrets get catalystcommunity/registry user
    )"
    CSILGEN_REGISTRY_PASSWORD="$(
        "$reactorcide_bin" secrets get catalystcommunity/registry password
    )"
}

clear_registry_credentials() {
    unset CSILGEN_REGISTRY_PASSWORD CSILGEN_REGISTRY_USER
    unset REACTORCIDE_SECRETS_PASSWORD
}

cleanup_registry_login() {
    if [[ -n "$CSILGEN_DOCKER_CONFIG" && -d "$CSILGEN_DOCKER_CONFIG" ]]; then
        find "$CSILGEN_DOCKER_CONFIG" -type f -delete
        rmdir "$CSILGEN_DOCKER_CONFIG"
    fi
    clear_registry_credentials
}

publish_ci_image() {
    local tag="$1"
    local digest_reference
    build_ci_image "$tag"
    load_registry_credentials

    CSILGEN_DOCKER_CONFIG="$(mktemp -d /tmp/csilgen-docker-config.XXXXXX)"
    trap cleanup_registry_login EXIT

    printf '%s' "$CSILGEN_REGISTRY_PASSWORD" \
        | DOCKER_CONFIG="$CSILGEN_DOCKER_CONFIG" docker login \
            "$CSILGEN_REGISTRY" \
            --username "$CSILGEN_REGISTRY_USER" \
            --password-stdin >/dev/null

    printf 'Publish %s:%s\n' "$CSILGEN_CI_IMAGE" "$tag"
    DOCKER_CONFIG="$CSILGEN_DOCKER_CONFIG" docker push "$CSILGEN_CI_IMAGE:$tag"
    printf 'Publish %s:latest\n' "$CSILGEN_CI_IMAGE"
    DOCKER_CONFIG="$CSILGEN_DOCKER_CONFIG" docker push "$CSILGEN_CI_IMAGE:latest"
    digest_reference="$(
        docker image inspect "$CSILGEN_CI_IMAGE:$tag" \
            --format '{{index .RepoDigests 0}}'
    )"
    printf 'Use this image in CI: %s\n' "$digest_reference"
}

command_name="${1:-}"
case "$command_name" in
    build-install-all)
        build_install_all
        ;;
    install-all)
        install_all
        ;;
    build-ci-image)
        build_ci_image "${2:-$(default_ci_image_tag)}"
        ;;
    publish-ci-image)
        publish_ci_image "${2:-$(default_ci_image_tag)}"
        ;;
    help|-h|--help|"")
        usage
        ;;
    *)
        printf 'Unknown command: %s\n' "$command_name" >&2
        usage >&2
        exit 1
        ;;
esac
