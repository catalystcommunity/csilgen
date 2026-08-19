#!/usr/bin/env bash

set -euo pipefail

CSILGEN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CSILGEN_CI_DOCKERFILE="$CSILGEN_ROOT/tools/ci-image/Dockerfile"
CSILGEN_CI_IMAGE="containers.catalystsquad.com/private/catalystcommunity/csilgen/ci-toolchain"
CSILGEN_REGISTRY="containers.catalystsquad.com"
CSILGEN_DOCKER_CONFIG=""

usage() {
    printf '%s\n' \
        "Usage: ./tools.sh build-ci-image [TAG]" \
        "       ./tools.sh publish-ci-image [TAG]" \
        "" \
        "build-ci-image builds the CSILgen CI toolchain for linux/amd64." \
        "publish-ci-image builds and publishes TAG and latest." \
        "The default TAG is derived from the Dockerfile content."
}

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'Required command is not available: %s\n' "$1" >&2
        exit 1
    fi
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
