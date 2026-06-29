#!/usr/bin/env bash
# Build the C# interop harness: it references the generated package (gen/, produced by the
# orchestrator) and the in-repo Csilgen.Transport library, and compiles to a single
# self-contained executable via `dotnet build`. Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "$HOME/.local/csilgen-tools/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

# A plain build restores the BCL-only project and compiles both project references. The
# generated package and the transport library carry zero NuGet dependencies, so this is
# offline after the SDK itself is present.
dotnet build csil-interop.csproj -c Release --nologo

echo "csharp harness built"
