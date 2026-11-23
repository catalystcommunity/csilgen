#!/usr/bin/env bash
# Generate every valid CSIL file in the repository for one target and compile
# the output with the target's strict checks. A spec that `csilgen validate`
# refuses is a negative fixture and is skipped. Usage: check.sh <target> [work-dir]
set -euo pipefail
fixture=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$fixture/../../.." && pwd)
cd "$root"
compiler=${CSILGEN:-"$root/target/release/csilgen"}
lang=${1:?supply a generator target}
work=${2:-$(mktemp -d)}
if [[ $# == 1 ]]; then trap 'rm -rf "$work"' EXIT; fi
mkdir -p "$work"
work=$(cd "$work" && pwd)

case "$lang" in
    rust)
        # The packages live outside the repository, where rustup does not see
        # `rust-toolchain.toml`. Use the pinned toolchain, which has rustfmt and clippy.
        # A host without rustup has one toolchain, so it needs no pin.
        if command -v rustup >/dev/null; then
            RUSTUP_TOOLCHAIN=$(rustup show active-toolchain | cut -d' ' -f1)
            export RUSTUP_TOOLCHAIN
        fi ;;
    swift) ;;
    *)
        echo "repo-specs: no compile check for $lang yet, skipped"
        exit 0 ;;
esac

# Harness and fixture `gen/` trees hold generated output, not specs.
mapfile -t specs < <(find . -name '*.csil' -not -path './target/*' \
    -not -path '*/node_modules/*' -not -path '*/gen/*' -not -path './.git/*' | sort)

generated=()
failed=0
n=0
for spec in "${specs[@]}"; do
    n=$((n + 1))
    if ! "$compiler" validate --input "$spec" >/dev/null 2>&1; then
        continue
    fi
    package="p$n"
    printf 'include "%s"\noptions { emit_packages: ["%s"], package_name: "%s", package_version: "0.1.0" }\n' \
        "$root/${spec#./}" "$lang" "$package" > "$work/$package.csil"
    # The CLI finds the local generator build relative to the working directory.
    if ! "$compiler" generate -q --input "$work/$package.csil" --target "$lang" \
        --output "$work/$package" >"$work/$package.log" 2>&1; then
        echo "FAIL generate $spec"
        sed 's/^/    /' "$work/$package.log"
        failed=$((failed + 1))
        continue
    fi
    echo "$spec" > "$work/$package/ORIGIN"
    generated+=("$package")
done
echo "repo-specs: generated ${#generated[@]} $lang packages"

cd "$work"
case "$lang" in
    rust)
        {
            echo '[workspace]'
            echo 'resolver = "2"'
            printf 'members = ['
            printf '"%s", ' "${generated[@]}"
            echo ']'
        } > Cargo.toml
        for package in "${generated[@]}"; do
            if ! out=$(rustfmt --check --edition 2021 "$package"/src/*.rs 2>&1); then
                echo "FAIL rustfmt $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done
        # One workspace compiles the shared dependencies once for all packages.
        if ! cargo clippy --quiet --workspace --all-targets -- -D warnings; then
            echo "FAIL clippy (see the errors above; each path names a pN package, and pN/ORIGIN names its spec)"
            failed=$((failed + 1))
        fi ;;
    swift)
        build=''
        for package in "${generated[@]}"; do
            # `emit_packages` puts the sources in SwiftPM's `Sources/<Target>/` layout.
            build+="swiftc -typecheck -warnings-as-errors $package/Sources/*/*.swift || { echo \"FAIL swiftc \$(cat $package/ORIGIN)\"; exit 1; }; "
        done
        if command -v swiftc >/dev/null; then
            sh -c "$build" || failed=$((failed + 1))
        else
            docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$work:$work" -w "$work" \
                swift:6.0-bookworm sh -c "$build" || failed=$((failed + 1))
        fi ;;
esac

if [[ $failed != 0 ]]; then
    echo "repo-specs: $failed $lang check(s) failed"
    exit 1
fi
echo "repo-specs: every valid spec compiles for $lang"
