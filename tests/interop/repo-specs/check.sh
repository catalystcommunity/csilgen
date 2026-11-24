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
    c) ;;
    swift | go | python | dart | ruby | php | java | kotlin | csharp | ocaml | elixir) ;;
    zig) ;;
    typescript)
        # One local install of the compiler serves every package.
        tsc_prefix="$work/.tsc"
        if ! npm install --silent --no-audit --no-fund --prefix "$tsc_prefix" typescript@5 >/dev/null 2>&1; then
            echo "FAIL npm install typescript@5"
            exit 1
        fi
        tsc="$tsc_prefix/node_modules/.bin/tsc" ;;
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
    c)
        # One translation unit includes every header, so two headers that declare
        # one name differently fail too.
        for package in "${generated[@]}"; do
            {
                for header in $(cd "$package" && find . -name '*.h' | sort); do
                    echo "#include \"$header\""
                done
                echo 'int main(void) { return 0; }'
            } > "$package/all.c"
            if ! out=$(cd "$package" && cc -std=c11 -Wall -Wextra -Werror -c all.c -o all.o 2>&1); then
                echo "FAIL cc $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
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
    go)
        # Each package has its own go.mod, so vet each one in its own directory.
        for package in "${generated[@]}"; do
            if ! out=$(cd "$package" && go vet ./... 2>&1); then
                echo "FAIL go vet $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    python)
        # Import every module, because a module that only the client imports can
        # still fail at import time. Then round-trip one sample value of each record:
        # the encoder must refuse a value (ValueError) or write bytes that its own
        # decoder gives back as the same value.
        cat > smoke.py <<'PY'
import dataclasses, datetime, decimal, enum, importlib, pathlib, sys, typing
sys.path.insert(0, ".")
name = sys.argv[1]
for module in sorted(pathlib.Path(name).glob("*.py")):
    importlib.import_module(name if module.stem == "__init__" else f"{name}.{module.stem}")
types = importlib.import_module(f"{name}.types")
SCALARS = {int: 0, str: "", bool: False, float: 0.0, bytes: b"",
           datetime.datetime: datetime.datetime(2020, 1, 1, tzinfo=datetime.timezone.utc),
           datetime.date: datetime.date(2020, 1, 1), decimal.Decimal: decimal.Decimal(0)}
def sample(tp, depth=0):
    origin, args = typing.get_origin(tp), typing.get_args(tp)
    if origin is typing.Union:
        return None if type(None) in args else sample(args[0], depth)
    if origin is typing.Literal:
        return args[0]
    if origin is tuple:
        return tuple(sample(a, depth) for a in args if a is not Ellipsis)
    if origin is list:
        return []
    if origin is dict:
        return {}
    if isinstance(tp, type) and issubclass(tp, enum.Enum):
        return next(iter(tp))
    if dataclasses.is_dataclass(tp):
        if depth > 8:
            raise RecursionError
        hints = typing.get_type_hints(tp, vars(types))
        return tp(**{f.name: sample(hints[f.name], depth + 1) for f in dataclasses.fields(tp)
                     if f.default is dataclasses.MISSING and f.default_factory is dataclasses.MISSING})
    return SCALARS.get(tp)
failures = 0
for rule, cls in vars(types).items():
    if not (isinstance(cls, type) and dataclasses.is_dataclass(cls)
            and cls.__module__ == types.__name__ and hasattr(cls, "to_cbor")):
        continue
    try:
        value = sample(cls)
    except Exception:
        continue
    try:
        encoded = value.to_cbor()
    except ValueError:
        continue
    except Exception as exc:
        failures += 1
        print(f"{rule}: encode raised {exc!r}")
        continue
    try:
        again = cls.from_cbor(encoded)
    except Exception as exc:
        failures += 1
        print(f"{rule}: decode of its own encoding raised {exc!r}")
        continue
    if again != value:
        failures += 1
        print(f"{rule}: round-trip gave {again!r}, want {value!r}")
sys.exit(1 if failures else 0)
PY
        for package in "${generated[@]}"; do
            if ! out=$(cd "$package" && python3 -I -W error ../smoke.py "$package" 2>&1); then
                echo "FAIL python $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | tail -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    dart)
        for package in "${generated[@]}"; do
            if ! out=$(cd "$package" && dart pub get --offline >/dev/null 2>&1 \
                && dart analyze --fatal-infos 2>&1); then
                echo "FAIL dart analyze $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    ruby)
        # Load every file of the gem in one process, so a constant that two files
        # declare differently fails too. `-W` turns on every warning; any warning fails.
        for package in "${generated[@]}"; do
            if ! out=$(cd "$package/lib" && ruby -W -e \
                'Dir["*.rb"].sort.each { |f| require_relative File.join(Dir.pwd, f) }' 2>&1) \
                || [[ "$out" == *"warning:"* ]]; then
                echo "FAIL ruby $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    typescript)
        for package in "${generated[@]}"; do
            if ! out=$(cd "$package" && "$tsc" -p tsconfig.json --noEmit 2>&1); then
                echo "FAIL tsc $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    php)
        # Load every file of the package with the transport in one process: PHP
        # compiles a class at load, and a name that two classes declare fails only
        # then. Any notice, warning or deprecation fails too.
        for package in "${generated[@]}"; do
            if ! out=$(php -d error_reporting=-1 -d display_errors=stderr -r '
                set_error_handler(function ($no, $msg) { fwrite(STDERR, "$msg\n"); exit(1); });
                require $argv[1] . "/transports/php/src/Errors.php";
                require $argv[1] . "/transports/php/src/CBOR.php";
                foreach (array("types", "codec", "client", "server") as $f) {
                    if (file_exists($argv[2] . "/src/$f.php")) require $argv[2] . "/src/$f.php";
                }' "$root" "$package" 2>&1); then
                echo "FAIL php $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    java)
        for package in "${generated[@]}"; do
            if ! out=$(javac -Xlint:all -Werror -d "$package/classes" \
                $(find "$package/src" -name '*.java') 2>&1); then
                echo "FAIL javac $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    kotlin)
        # One Gradle build with a subproject per package compiles them all in one run.
        # The packages pin their own plugin version, so the subprojects take only the
        # sources and use the plugin the harness caches.
        mkdir -p kt
        : > kt/settings.gradle.kts
        for package in "${generated[@]}"; do
            mkdir -p "kt/$package"
            ln -sfn "$work/$package/src" "kt/$package/src"
            echo "include(\"$package\")" >> kt/settings.gradle.kts
        done
        cat > kt/build.gradle.kts <<'KTS'
plugins { kotlin("jvm") version "2.2.20" apply false }
subprojects {
    apply(plugin = "org.jetbrains.kotlin.jvm")
    repositories { mavenCentral() }
    extensions.configure<org.jetbrains.kotlin.gradle.dsl.KotlinJvmProjectExtension> {
        compilerOptions { allWarningsAsErrors.set(true) }
    }
}
KTS
        # Eighty compilations in one build need more than the default heaps.
        printf 'org.gradle.jvmargs=-Xmx3g\nkotlin.daemon.jvmargs=-Xmx3g\nkotlin.compiler.execution.strategy=in-process\n' \
            > kt/gradle.properties
        if ! (cd kt && gradle --offline --console=plain -q --continue --max-workers=2 compileKotlin); then
            echo "FAIL kotlinc (see the errors above; each path names a pN package, and pN/ORIGIN names its spec)"
            failed=$((failed + 1))
        fi ;;
    ocaml)
        # One dune workspace builds every package (each has its own dune-project and
        # library name) in one run. The default dev profile makes warnings errors.
        echo '(lang dune 3.0)' > dune-workspace
        if ! out=$(dune build --root . 2>&1); then
            echo "FAIL dune build (each path names a pN package, and pN/ORIGIN names its spec)"
            printf '%s\n' "$out" | head -60 | sed 's/^/    /'
            failed=$((failed + 1))
        fi ;;
    elixir)
        # Compile each package on its own, so modules of two packages never meet.
        for package in "${generated[@]}"; do
            if ! out=$(elixirc --warnings-as-errors -o "$package/_beam" "$package"/lib/*.ex 2>&1); then
                echo "FAIL elixirc $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    zig)
        # Zig analyzes only what is referenced, so a test that references every
        # declaration of every generated file makes the compiler check all of it.
        # It descends only into types the file declares itself: descending into a
        # std type (a map alias) references std internals that crash the test.
        for package in "${generated[@]}"; do
            {
                cat <<'ZIG'
const std = @import("std");
fn refOwn(comptime T: type) void {
    @setEvalBranchQuota(1_000_000);
    inline for (comptime std.meta.declarations(T)) |decl| {
        const value = @field(T, decl.name);
        if (@TypeOf(value) == type) switch (@typeInfo(value)) {
            .@"struct", .@"enum", .@"union", .@"opaque" => if (comptime std.mem.startsWith(u8, @typeName(value), @typeName(T) ++ ".")) refOwn(value),
            else => {},
        };
        _ = &@field(T, decl.name);
    }
}
ZIG
                echo 'test {'
                for file in "$package"/*.gen.zig; do
                    echo "    refOwn(@import(\"$(basename "$file")\"));"
                done
                echo '}'
            } > "$package/all_decls_test.zig"
            if ! out=$(zig test --cache-dir "$work/.zig-cache" "$package/all_decls_test.zig" 2>&1); then
                echo "FAIL zig test $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
    csharp)
        export DOTNET_NOLOGO=1 DOTNET_CLI_TELEMETRY_OPTOUT=1
        for package in "${generated[@]}"; do
            if ! out=$(cd "$package" && dotnet build -warnaserror -nologo -v q 2>&1); then
                echo "FAIL dotnet build $(cat "$package/ORIGIN")"
                printf '%s\n' "$out" | grep -E 'error|warning' | sort -u | head -20 | sed 's/^/    /'
                failed=$((failed + 1))
            fi
        done ;;
esac

if [[ $failed != 0 ]]; then
    echo "repo-specs: $failed $lang check(s) failed"
    exit 1
fi
echo "repo-specs: every valid spec compiles for $lang"
