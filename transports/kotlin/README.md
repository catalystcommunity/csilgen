# csilgen-transport (Kotlin/JVM)

A first-class Kotlin implementation of the CSIL transport family — CSIL-RPC,
CSIL-Events, and CSIL-Datagrams — over a hand-rolled **canonical CBOR** codec.
Matched client and server for all three transports, a bring-your-own-carrier seam,
and byte-for-byte conformance with the shared vectors in `transports/conformance/`.

- **Package**: `community.catalyst.csilgen.transport`
- **Coordinates**: `community.catalyst.csilgen:csilgen-transport:0.1.0`
- **Kotlin**: 2.2+ (K2), language/runtime floor is Kotlin 2.0
- **Toolchain**: JDK 17 (LTS)
- **Runtime dependencies**: **none** beyond the Kotlin stdlib. The CBOR codec is
  hand-written; there is no CBOR library, no kotlinx-serialization, and **no coroutines**.

## Design invariants

- **Synchronous, blocking, no coroutines — ever.** Carrier I/O is plain
  `InputStream`/`OutputStream`. The host owns the I/O loop and any threads; wrap a
  server in a `Thread`/`ExecutorService` if you want concurrency. Do not suspend-ify
  the API.
- **Canonical CBOR.** Maps are encoded with entries sorted by the *bytewise
  lexicographic order of each key's encoded bytes* (shorter-first), so the same
  logical envelope always yields identical bytes. The wire is unsigned: scalars use
  Kotlin's stable inline `ULong`/`UByte`, but plain `ByteArray` is used on the wire
  (the unsigned *array* types are still beta).
- **Bring your own carrier.** Implement `FrameCarrier` (RPC/Events) or
  `DatagramCarrier` (Datagrams); `recv…(): ByteArray?` returning `null` is the clean
  end-of-stream signal. Built-ins: `StreamCarrier` (4-byte big-endian length-prefix
  framing with a 16 MiB frame guard), `LoopbackFrameCarrier`/`LoopbackDatagramCarrier`
  (in-memory, for tests), and `UdpDatagramCarrier`.
- **Value equality for payloads.** Every envelope holding a `ByteArray` overrides
  `equals`/`hashCode` with `contentEquals` — a `data class`'s default would compare
  array *identity* and silently fail the decode round-trip.

## Layout

```
transports/kotlin/
├── build.gradle.kts / settings.gradle.kts / gradle.properties
├── gradlew / gradlew.bat / gradle/wrapper/   # committed Gradle 8.14.3 wrapper
├── run-tests.sh                              # ./gradlew test (used by xtask)
└── src/
    ├── main/kotlin/community/catalyst/csilgen/transport/
    │   ├── Cbor.kt          # canonical encode/decode (hand-rolled, zero deps)
    │   ├── Conventions.kt   # VERSION, Status, tag-24, frame guard, exceptions, map helpers
    │   ├── Carrier.kt       # FrameCarrier/DatagramCarrier seams + stream/loopback built-ins
    │   ├── Udp.kt           # UdpDatagramCarrier
    │   ├── Rpc.kt           # RpcRequest/Response/Push, RpcClient, RpcServer
    │   ├── Events.kt        # Event (verbose/compact), Hello/HelloAck/Heartbeat/Close
    │   ├── Datagrams.kt     # Datagram, CompactDatagram, SeqTracker
    │   └── Transport.kt     # package entrypoint / library metadata
    └── test/kotlin/community/catalyst/csilgen/transport/
        ├── ConformanceTest.kt   # drives transports/conformance/*.json
        ├── RoundtripTest.kt     # client/server, framing guards, seq tracker
        └── Json.kt              # tiny dependency-free JSON parser for the vectors
```

## Building and testing

The only system requirement is a **JDK 17+** on `PATH` (`java`/`javac`). Everything
else — the Gradle distribution, the Kotlin stdlib, JUnit 5 — is fetched by the
committed wrapper on first run.

```bash
./gradlew test          # or: ./run-tests.sh
```

The first run needs network access (to fetch the Gradle distribution + dependencies);
subsequent runs work with `./gradlew --offline test`. This is the same constraint that
npm/pip have.

## Consuming this library from git (no published artifact)

> **TODO(publish):** Gradle has no native git-dependency mechanism the way Go modules
> and Cargo do — it resolves from Maven/Ivy repositories, not arbitrary git URLs. Until
> we publish to Maven Central, consume the library via a git submodule + composite build
> (below). Publishing later means adding the `maven-publish` plugin, a publication from
> `components["java"]` with `sources`/`javadoc` jars, GPG signing, and pushing to the
> Maven Central Portal; consumers would then replace `includeBuild` with a plain
> `implementation("community.catalyst.csilgen:csilgen-transport:<version>")`.

**Composite build via git submodule (works today, no publish step):**

1. Add this repo as a submodule in your consumer project:
   ```bash
   git submodule add <csilgen-repo-url> third_party/csilgen
   git submodule update --init
   ```
2. In your consumer `settings.gradle.kts`:
   ```kotlin
   includeBuild("third_party/csilgen/transports/kotlin")
   ```
3. In your consumer `build.gradle.kts`:
   ```kotlin
   dependencies {
       implementation("community.catalyst.csilgen:csilgen-transport")
   }
   ```

Gradle substitutes that coordinate with the locally-built source — nothing is
published, not even to `mavenLocal`, and changes are picked up on the next build. The
declared `group`/`rootProject.name` match the coordinate above, so no
`dependencySubstitution { … }` block is needed. Composite builds require compatible
Gradle versions between the two builds; the committed wrapper pins Gradle 8.14.3.
