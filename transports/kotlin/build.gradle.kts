// CSIL transport library for Kotlin/JVM. First-class idiomatic Kotlin, stdlib only at
// runtime (the hand-rolled CBOR codec means zero third-party runtime deps), JDK 17
// toolchain. No coroutines, no kotlinx-serialization — by design.
//
// TODO(publish): Gradle cannot consume a library straight from a git URL the way Cargo/Go
// can. Until we publish, consumers use a git submodule + `includeBuild(...)` (a composite
// build); see README.md. To publish for coordinate-based consumption, add the
// `maven-publish` plugin, define a publication from `components["java"]` with `sources`/
// `javadoc` jars, and push to Maven Central (Central Portal / Sonatype, requiring a
// verified namespace and GPG signing). Then consumers replace `includeBuild` with
// `implementation("community.catalyst.csilgen:csilgen-transport:<version>")`.

plugins {
    kotlin("jvm") version "2.2.20"
}

group = "community.catalyst.csilgen"
version = "0.1.0"

repositories {
    // Needed only to fetch the Kotlin stdlib and the test engine; there are no third-party
    // runtime dependencies.
    mavenCentral()
}

dependencies {
    testImplementation(kotlin("test"))
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.4")
}

kotlin {
    // Gradle locates an installed JDK 17 (declared as the lone system dependency in the
    // README); it does not rely on toolchain auto-download.
    jvmToolchain(17)
}

tasks.test {
    useJUnitPlatform()
}
