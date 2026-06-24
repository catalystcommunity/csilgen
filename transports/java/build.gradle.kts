// The CSIL Java transport library: hand-written, zero runtime dependencies, synchronous.
// `maven-publish` is applied so JitPack can install the artifact straight from a git tag
// (see README); the published jar carries no dependencies — JUnit is test-scope only.
plugins {
    `java-library`
    `maven-publish`
}

group = "community.catalyst.csilgen"
version = "0.1.0"

repositories {
    mavenCentral()
}

java {
    // Compile against the Java 17 floor so the artifact runs on any JDK >= 17, while a
    // contributor may build with a newer toolchain. withSourcesJar feeds JitPack/Central.
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
    withSourcesJar()
}

dependencies {
    testImplementation(platform("org.junit:junit-bom:5.10.2"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        events("passed", "failed", "skipped")
    }
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            from(components["java"])
            artifactId = "csilgen-transport"
        }
    }
}
