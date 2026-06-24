#!/usr/bin/env sh
# Configure, build, and run the csil_transport tests under ASan/UBSan, returning
# non-zero on any failure. CMake is a multi-step tool (configure + build + test),
# so the xtask runner shells out to this single script. Required toolchain: a C11
# compiler (cc) and cmake (>= 3.18).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
build="$here/build-test"

# Sanitizers are cheap here and catch the lurking UB (strict aliasing, overflow,
# use-after-free) that is otherwise silent until a compiler upgrade breaks the wire.
sanflags="-fsanitize=address,undefined -fno-sanitize-recover=all"

cmake -S "$here" -B "$build" \
    -DCMAKE_BUILD_TYPE=Debug \
    -DBUILD_TESTING=ON \
    -DCMAKE_C_FLAGS="$sanflags" >/dev/null

cmake --build "$build" >/dev/null

ctest --test-dir "$build" --output-on-failure
