# Run csilgen CI on your computer

Install Docker. Build the reactorcide executable from the reactorcide
repository. The examples below assume that the reactorcide repository and this
repository have the same parent directory.

Run one job with this command:

```text
../reactorcide/coordinator_api/reactorcide run-local \
  .reactorcide/jobs/test-core.yaml
```

Replace `test-core.yaml` with one of these job files:

- `conventional-commits.yaml` checks commit subjects.
- `test-core.yaml` checks and tests the Rust workspace.
- `test-generators.yaml` builds all production generator WASM modules.
- `test-transports.yaml` tests all transport libraries.
- `test-interop.yaml` runs one cross-language interoperability shard.
- `package.yaml` builds all release archives. It does not publish a release.
- `asset-build.yaml` builds one cache asset from a workflow item.

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

The pull request workflow expands the interoperability job into four shards.
Each shard tests all clients and transports against a group of server
languages. The workflow succeeds only when all shards succeed. The job timeout
stops a shard that does not finish.

## Releases

The release-tag workflow runs after a pull request merges to `main`. It uses
`semver-tags` v0.6.0 and Conventional Commits. It calculates one `csilgen`
version for all files in the repository.

The commit type defines the version change. A `feat` commit changes the minor
version. A breaking commit changes the major version. Other supported commit
types change the patch version. The commit scope does not select a release
target.

The release-tag job creates one draft GitHub Release. It promotes the sealed
assets from the merged pull request when the source tree is the same. It then
pushes one `csilgen/vX.Y.Z` tag. If the cache is absent or stale, the tag
workflow builds the assets again.

The pull request and tag workflows build assets in parallel. An asset job gets
signed staging URLs. It does not get the bucket keys.
A trusted control job verifies each digest and copies the object to a sealed
key. The release job verifies the draft, source commit, source tree, and sealed
objects. It then uploads:

- Four separate CLI archives
- One archive that contains all production generator WASM modules
- One separate source archive for each transport

The final workflow job runs for all results. It keeps the last six complete
version lanes and their referenced pull request and main lanes. It deletes
older lanes below the `csilgen/` prefix.

The remote release jobs read the GitHub token from the reactorcide
`catalystcommunity/ci:githubpat` secret. Trusted asset jobs read the isolated
`catalystcommunity/asset-cache` secret. You cannot run these jobs on your
computer. Use `package.yaml` to test all release builds without a publish
action.
