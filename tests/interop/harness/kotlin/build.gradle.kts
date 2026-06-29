// Kotlin interop harness: links the generated package (gen/) and the monorepo's Kotlin
// transport library (via the composite build in settings.gradle.kts), and produces a
// runnable launcher (installDist) so `run` starts without Gradle daemon overhead.

plugins {
    kotlin("jvm") version "2.2.20"
    application
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("community.catalyst.csilgen:csilgen-transport:0.1.0")
}

kotlin {
    jvmToolchain(17)
}

// The generated CSIL package is compiled straight into the harness rather than published.
sourceSets {
    main {
        kotlin.srcDir("gen/src/main/kotlin")
    }
}

application {
    mainClass.set("csil.interop.MainKt")
}
