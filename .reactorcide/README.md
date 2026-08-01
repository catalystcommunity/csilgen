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

The YAML job command starts runnerlib. Remote jobs load the Python lifecycle
plugin from the trusted CI checkout of `main`. The tested source stays in a
separate checkout. Runnerlib does not load a plugin from the tested source when
the trusted checkout is present.

The lifecycle plugin is
[`plugins/plugin_csilgen_jobs.py`](plugins/plugin_csilgen_jobs.py). The trusted
interop implementation is [`scripts/interop.py`](scripts/interop.py). The
plugin runs the selected job in the `POST_SOURCE_PREP` phase. A Python error
causes the runnerlib job to fail.

The interoperability job is separate from the other test jobs. Its runtime
limit is 360 seconds. The job fails if the test command takes more time.

## Releases

The release-tag workflow runs after a pull request merges to `main`. It uses
`semver-tags` v0.6.0 and Conventional Commits. It calculates a separate version
for the CLI, each generator, and each transport.

The `.semver-tags.yaml` file defines named targets and their paths. A commit can
change more than one target. A change to a shared path can release all targets
that use that path. The target name defines the tag prefix. The source path
does not define the public name.

The commit type defines the version change. A `feat` commit changes the minor
version. A breaking commit changes the major version. Other supported commit
types change the patch version. The commit scope does not select a release
target. The changed paths select the targets.

The release-tag job creates one draft GitHub Release for each changed target.
It then pushes all new tags in one atomic operation. Tags have these forms:

- `csilgen-core/vX.Y.Z`
- `generator-<language>/vX.Y.Z`
- `transport-<language>/vX.Y.Z`

Each tag starts a separate release workflow. The release job verifies the
draft and the source commit before it builds an artifact. It then uploads:

- Four CLI archives for a `csilgen-core` tag
- One WASM archive for a generator tag
- One source archive for a transport tag

The specifications and tests do not have release targets. The release jobs do
not publish transport archives to package registries.

The remote release jobs read the GitHub token from the reactorcide
`catalystcommunity/ci:githubpat` secret. You cannot run these jobs on your
computer. Use `package.yaml` to test all release builds without a publish
action.
