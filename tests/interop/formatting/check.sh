#!/usr/bin/env bash
# Generate stress.csil and every valid CSIL file in the repository as a package for
# one target, then run the toolchain formatter of that target in check mode with its
# default settings. Generated files that consumers check in must not change when the
# consumer runs its formatter. Usage: check.sh <target> [work-dir]
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
        if command -v rustup >/dev/null; then
            RUSTUP_TOOLCHAIN=$(rustup show active-toolchain | cut -d' ' -f1)
            export RUSTUP_TOOLCHAIN
        fi ;;
    go | zig | dart | elixir | csharp | swift) ;;
    *)
        echo "formatting: $lang has no toolchain formatter, skipped"
        exit 0 ;;
esac

mapfile -t specs < <(
    echo "./tests/interop/formatting/stress.csil"
    find . -name '*.csil' -not -path './target/*' -not -path '*/node_modules/*' \
        -not -path '*/gen/*' -not -path './.git/*' \
        -not -path './tests/interop/formatting/*' | sort
)

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
    if ! "$compiler" generate -q --input "$work/$package.csil" --target "$lang" \
        --output "$work/$package" >"$work/$package.log" 2>&1; then
        echo "FAIL generate $spec"
        sed 's/^/    /' "$work/$package.log"
        failed=$((failed + 1))
        continue
    fi
    echo "$spec" > "$work/$package.origin"
    generated+=("$package")
done
echo "formatting: generated ${#generated[@]} $lang packages"

report() {
    echo "FAIL $1 $(cat "$work/$2.origin")"
    printf '%s\n' "$3" | sed -n '1,30s/^/    /p'
    failed=$((failed + 1))
}

cd "$work"
swift_script=''
for package in "${generated[@]}"; do
    case "$lang" in
        rust)
            # A consumer crate can use either style edition.
            for edition in 2021 2024; do
                files=$(find "$package" -name '*.rs')
                if [[ -n $files ]] && ! out=$(rustfmt --check --edition "$edition" $files 2>&1); then
                    report "rustfmt --edition $edition" "$package" "$out"
                fi
            done ;;
        go)
            # gofmt exits non-zero when it cannot parse a file; report that too.
            if ! out=$(gofmt -l "$package" 2>&1) || [[ -n $out ]]; then
                report gofmt "$package" "$out
$(gofmt -d "$package" 2>&1 || true)"
            fi ;;
        zig)
            if ! out=$(zig fmt --check "$package" 2>&1); then report "zig fmt" "$package" "$out"; fi ;;
        dart)
            if ! out=$(dart format --output=none --set-exit-if-changed "$package" 2>&1); then
                report "dart format" "$package" "$out"
            fi ;;
        elixir)
            # Check with the `.formatter.exs` that the generator emits beside the files.
            dot=$(find "$package" -name .formatter.exs | head -1)
            if [[ -z $dot ]]; then report "mix format" "$package" "no .formatter.exs emitted"; continue; fi
            dir=$(dirname "$dot")
            if ! out=$(cd "$dir" && mix format --check-formatted --dot-formatter .formatter.exs \
                $(find . -name '*.ex' -o -name '*.exs' | grep -v '^./.formatter.exs$') 2>&1); then
                report "mix format" "$package" "$out"
            fi ;;
        csharp)
            if ! out=$(dotnet format whitespace --folder "$package" --verify-no-changes 2>&1); then
                report "dotnet format whitespace" "$package" "$out"
            fi ;;
        swift)
            # swift-format has no check mode: format a copy and compare.
            swift_script+="cp -r $package $package.fmt && swift format format --recursive --in-place $package.fmt \
&& { diff -ru $package $package.fmt || { echo \"FAIL swift format \$(cat $package.origin)\"; exit 1; }; }; "
            ;;
    esac
done
if [[ $lang == swift && -n $swift_script ]]; then
    if command -v swift >/dev/null; then
        sh -c "$swift_script" || failed=$((failed + 1))
    else
        docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$work:$work" -w "$work" \
            swift:6.0-bookworm sh -c "$swift_script" || failed=$((failed + 1))
    fi
fi

if [[ $failed != 0 ]]; then
    echo "formatting: $failed $lang check(s) failed"
    exit 1
fi
echo "formatting: every generated $lang package passes its formatter"
