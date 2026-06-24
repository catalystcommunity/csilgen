#!/usr/bin/env bash
# Full build + test for the C# transport library. xtask invokes this when the .NET SDK is present
# and skips it otherwise. Exits non-zero on any failure.
set -euo pipefail

cd "$(dirname "$0")"

# `dotnet test` restores (xunit dev-deps), builds both projects via the solution, and runs the
# tests. The first run hits nuget.org to restore the test-only packages; later runs are offline.
exec dotnet test Csilgen.Transport.sln --nologo
