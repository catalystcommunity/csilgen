# Run csilgen CI on your computer

Install Docker. Build the reactorcide executable from the reactorcide
repository. The examples below assume that the reactorcide repository and this
repository have the same parent directory.

Run one job with this command:

```text
../reactorcide/coordinator_api/reactorcide run-local \
  --job-dir . \
  .reactorcide/jobs/test-core.yaml
```

Replace `test-core.yaml` with one of these job files:

- `conventional-commits.yaml` checks commit subjects.
- `test-core.yaml` checks and tests the Rust workspace.
- `test-generators.yaml` builds all production generator WASM modules.
- `test-transports.yaml` tests all transport libraries.
- `test-interop.yaml` runs the cross-language interoperability matrix.
- `package.yaml` builds all release archives. It does not publish a release.

The transport and interoperability jobs build a CI image before they run the
tests. This build can take several minutes. The package job can pull
cross-build images.

All job logic is in
[`plugins/plugin_csilgen_jobs.py`](plugins/plugin_csilgen_jobs.py). The YAML job
command starts runnerlib. Runnerlib loads the plugin after it prepares the
source. The plugin then runs the selected job in the `POST_SOURCE_PREP` phase.
A plugin error causes the runnerlib job to fail.

The interoperability job is separate from the other test jobs. Its runtime
limit is 360 seconds. The job fails if the test command takes more time.

## Releases

The release workflow runs after a pull request merges to `main`. It uses
`semver-tags` and Conventional Commits to calculate one repository version. It
creates a tag with the form `vX.Y.Z`.

The release job creates one GitHub Release for the tag. It uploads:

- CLI archives for Linux x86-64, Linux ARM64, macOS ARM64, and Windows x86-64
- A separate WASM archive for each production generator
- A separate source archive for each transport library

All archives use the same repository version. The specifications and tests are
not release artifacts. Transport archives are not published to package
registries.

The remote release job reads the GitHub token from the reactorcide
`catalystcommunity/ci:githubpat` secret. Do not run `release.yaml` on your
computer. Use `package.yaml` to test release builds without a publish action.
