// The harness depends on the monorepo's Kotlin transport library via a Gradle composite
// build (the documented consumption path until the lib is published), so a local edit to
// the transport is picked up without a separate install step.
rootProject.name = "csil-interop-kotlin"

includeBuild("../../../../transports/kotlin")
