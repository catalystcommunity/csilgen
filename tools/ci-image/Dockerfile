FROM docker.io/library/node:24-bookworm-slim AS node
FROM mcr.microsoft.com/dotnet/sdk:8.0-bookworm-slim AS dotnet
FROM docker.io/library/dart:stable AS dart
FROM docker.io/library/swift:6.0-bookworm AS swift
FROM docker.io/library/eclipse-temurin:17-jdk AS java
FROM docker.io/library/gradle:8.14.3-jdk17 AS gradle
FROM docker.io/library/debian:bookworm-slim AS zig

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates xz-utils \
    && rm -rf /var/lib/apt/lists/*
ADD https://ziglang.org/download/0.14.1/zig-x86_64-linux-0.14.1.tar.xz /tmp/zig.tar.xz
RUN echo "24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c  /tmp/zig.tar.xz" \
        | sha256sum --check --strict \
    && mkdir /opt/zig \
    && tar -xJf /tmp/zig.tar.xz --strip-components=1 -C /opt/zig

FROM containers.catalystsquad.com/public/reactorcide/runnerbase:latest

LABEL org.opencontainers.image.source="https://github.com/catalystcommunity/csilgen"
LABEL org.opencontainers.image.description="CSILgen interoperability and transport test toolchain"

USER root

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        cmake \
        elixir \
        erlang-dev \
        libicu76 \
        ocaml-dune \
        ocaml \
        php-cli \
        ruby-full \
    && rm -rf /var/lib/apt/lists/*

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
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

RUN gem install --no-document json minitest \
    && rustup target add wasm32-unknown-unknown \
    && mkdir -p /job/src /job/ci
