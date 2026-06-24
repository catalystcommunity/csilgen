# csilgen-transport (Java)

A hand-written, **zero-runtime-dependency**, synchronous Java implementation of the CSIL
transport family — CSIL-RPC, CSIL-Events, and CSIL-Datagrams — over a canonical CBOR codec
owned by this library. It mirrors the reference libraries in `transports/go/`,
`transports/python/`, and `transports/rust/` and is verified byte-for-byte against the
shared conformance vectors in `transports/conformance/`.

Package: `community.catalyst.csilgen.transport`.

## Design

- **No async, ever.** Every API is blocking. The host owns the I/O loop and supplies a
  carrier (the bring-your-own-carrier seam: `FrameCarrier` / `DatagramCarrier`). Concurrency,
  if a host wants it, is the host's threads around the blocking seam — the library never
  spawns and exposes no `CompletableFuture`/reactive types.
- **One source of truth for bytes.** `Cbor.java` is a minimal canonical CBOR codec (RFC 8949
  core deterministic encoding: map entries sorted by the bytewise-unsigned order of their
  *encoded* keys). It supports exactly what the envelopes need — unsigned/negative ints, text
  and byte strings, arrays, maps, and tag 24 — and nothing else.
- **Java 17 idioms.** Sealed `CborValue` with a `record` per variant; envelopes are `record`s;
  exhaustive `switch`. Records carrying a `byte[]` (payloads, bodies) override
  `equals`/`hashCode` with `Arrays.equals` so decode→equality holds (a record's generated
  `equals` compares arrays by reference, which would otherwise spuriously fail conformance).
- **Unsigned 64-bit discipline.** Java has no unsigned types, so the codec uses
  `Long.compareUnsigned`/`>>>`/`Arrays.compareUnsigned` and rejects CBOR negative integers
  below the signed-64 floor, exactly as the Go reference does.
- **16 MiB frame guard** on length-prefixed stream reads, enforced before allocating.

## Layout

```
transports/java/
├── build.gradle.kts / settings.gradle.kts   # Gradle build (maven-publish applied)
├── pom.xml                                   # equivalent Maven build
├── gradlew / gradlew.bat / gradle/wrapper/   # committed Gradle wrapper (Gradle 8.7)
├── run-tests.sh                              # ./gradlew test (used by xtask)
└── src/
    ├── main/java/community/catalyst/csilgen/transport/
    │   ├── Cbor.java, CborValue.java         # canonical CBOR codec + value model
    │   ├── Conventions.java, Status.java     # version, tag-24, status registry, accessors
    │   ├── TransportException.java + subclasses
    │   ├── FrameCarrier.java, DatagramCarrier.java, Carriers.java, UdpDatagramCarrier.java
    │   ├── Rpc.java, Events.java, Datagrams.java
    └── test/java/.../                        # ConformanceTest, RoundtripTest (+ JSON/Vectors helpers)
```

## Toolchain

- **Runtime floor: JDK 17.** The artifact is compiled with `--release 17`, so it runs on any
  JDK >= 17.
- **Building/testing:** a JDK (>= 17) is the only required tool. The committed Gradle wrapper
  downloads its pinned Gradle (8.7) on first run, so no system Gradle install is needed —
  only a JDK and network access on first run. (Gradle 8.7 runs on JDK 17–21; if you build
  with a newer JDK than the wrapper supports, bump the wrapper version.)

## Running the tests

```sh
cd transports/java
./run-tests.sh        # equivalently: ./gradlew test
```

The conformance test reconstructs each vector's envelope from its language-neutral `input`,
asserts `encode → hex`, and asserts `decode(hex) → equal envelope`. The vectors are read by
walking up to `transports/conformance/`, so the test works whether launched from this
directory or the repo root. JUnit 5 is a **test-scope-only** dependency; the published
library jar has no dependencies (the test JSON reader is hand-rolled in `Json.java`).

A pure-JDK fallback without Gradle is also possible (`javac` + the JUnit
`junit-platform-console-standalone` jar), but the wrapper is the supported path.

## Consuming this library

**TODO (publishing): this library is not yet published to Maven Central, and neither Maven
nor Gradle can depend on a *subdirectory of a git repo with no published artifact* without a
publish step.** Until a release is cut:

- **JitPack (interim path).** Add the JitPack repository and depend on the module via the
  multi-module coordinate. Requires a **git tag** (or commit hash), a **public** repo, and
  the `maven-publish` plugin this build already applies.

  Gradle:
  ```kotlin
  repositories { maven { url = uri("https://jitpack.io") } }
  dependencies {
      implementation("com.github.catalystcommunity.csilgen:csilgen-transport:<TAG>")
  }
  ```
  Maven:
  ```xml
  <repository><id>jitpack.io</id><url>https://jitpack.io</url></repository>
  <dependency>
    <groupId>com.github.catalystcommunity.csilgen</groupId>
    <artifactId>csilgen-transport</artifactId>
    <version>TAG</version>
  </dependency>
  ```
  (Group is `com.github.<User>.<Repo>`; artifact is this module's name. First resolution
  triggers a one-time remote build on JitPack.)

- **Maven Central (future work).** The only way for arbitrary downstreams to depend with zero
  extra repository config. Requires a registered Sonatype Central namespace
  (`io.github.<user>` or an owned group), GPG-signed artifacts, and sources + javadoc jars,
  plus a release pipeline. Tracked as future work.

- **Gradle source dependencies** (`sourceControl { gitRepository(...) }`) are possible only if
  a root-level Gradle build is added to this Rust-workspace repo that exposes this subproject;
  not wired up yet.

- **Local fallback today:** `./gradlew publishToMavenLocal` (or `mvn install`) into `~/.m2`,
  then depend on `community.catalyst.csilgen:csilgen-transport:0.1.0` normally. Works offline
  but is not "straight from git".
