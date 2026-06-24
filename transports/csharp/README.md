# Csilgen.Transport (C#)

Reference C# implementation of the CSIL transport family: **CSIL-RPC**, **CSIL-Events**, and
**CSIL-Datagrams**, over a hand-rolled canonical-CBOR codec. It owns the envelope codecs, framing,
and connection lifecycle; the byte/datagram **carrier** is injected (bring-your-own-carrier), so a
host plugs HTTP, WebSocket, QUIC, WebRTC, or a platform media stack without changing this library.

Target framework: **`net8.0`** (modern .NET / CoreCLR — **not** .NET Framework). C# 12. The shipped
library has **zero NuGet dependencies**: the CBOR codec is hand-rolled on `System.Buffers.Binary`,
`System.Text.Encoding`, and `Span<byte>`, matching the repo's zero-dependency house rule (see the
header of `src/Csilgen.Transport/Cbor.cs` for why `System.Formats.Cbor` was declined).

The byte layout is pinned by the conformance vectors in `transports/conformance/`; the test suite
verifies every vector.

## Layout

```
transports/csharp/
├── Csilgen.Transport.sln
├── Directory.Build.props                       # shared net8.0 / Nullable / warnings-as-errors
├── src/Csilgen.Transport/                      # the shipped library (zero PackageReferences)
│   ├── Cbor.cs        Conventions.cs  Carrier.cs
│   └── Rpc.cs         Events.cs       Datagrams.cs
└── tests/Csilgen.Transport.Tests/              # xUnit; the only project with NuGet refs
    ├── ConformanceTests.cs                     # encode==hex / decode==envelope for every vector
    └── RoundtripTests.cs
```

Everything is **synchronous and blocking** — there is no `async`/`await`, `Task`, or `*Async` call
anywhere, by house rule. Concurrency, where ever needed, would use `System.Threading.Thread`.

## Required toolchain

The **only** dependency is the .NET SDK **8.0 or newer** on `PATH` (it bundles MSBuild, the
compiler, and the runtime — no Visual Studio, no system MSBuild, no mono). Probe it with:

```sh
dotnet --version    # prints e.g. 8.0.404 and exits 0 when the SDK is present
```

No other system packages are required.

## Running the tests

```sh
transports/csharp/run-tests.sh      # == cd transports/csharp && dotnet test Csilgen.Transport.sln
```

The first `dotnet test` restores the **test-only** packages (`xunit`,
`xunit.runner.visualstudio`, `Microsoft.NET.Test.Sdk`) from nuget.org — the one network step,
consistent with how the TypeScript lib's `npm test` restores devDeps. Subsequent runs are offline.
The shipped library itself restores nothing.

## Consuming it straight from this git repo

**NuGet has no git source.** `PackageReference` resolves only from NuGet *feeds* (nuget.org, a
private feed, or a local folder of `.nupkg` files) — there is no `PackageReference Include="git…"`
equivalent of a Go-module or Cargo git dependency. So consuming from git is done with a
**source/project reference**, not a package reference:

1. Add this repo as a git submodule in the consumer, e.g. under `external/csilgen`:

   ```sh
   git submodule add <repo-url> external/csilgen
   ```

2. Reference the library project by path from the consumer's `.csproj`:

   ```xml
   <ProjectReference Include="../external/csilgen/transports/csharp/src/Csilgen.Transport/Csilgen.Transport.csproj" />
   ```

`dotnet build` / `dotnet test` then compile the transport from source as part of the consumer's
build. A vendored (checked-in) copy of `transports/csharp/` works identically via the same
relative-path `ProjectReference` — no submodule required. This is the fully-working, zero-publish
path, parallel to how Go/Cargo/npm consume from git.

### What does **not** work
- `PackageReference` to a git URL — unsupported, full stop.
- `dotnet add package Csilgen.Transport` — fails until we publish a `.nupkg` to a feed.
- Referencing a sub-directory of a repo *as a package* — only as a project/source path.

### TODO(nuget)

```
TODO(nuget): Publish Csilgen.Transport as a NuGet package so consumers can use
<PackageReference Include="Csilgen.Transport" Version="x.y.z" /> instead of a
submodule + ProjectReference. Requires:
  1. Set <PackageId>, <Version>, <Authors>, <Description>, <RepositoryUrl>,
     <PackageLicenseExpression>Apache-2.0</PackageLicenseExpression> in the csproj.
  2. `dotnet pack -c Release` to produce the .nupkg.
  3. Publish to nuget.org (`dotnet nuget push`) OR to a GitHub Packages NuGet
     feed (github.com/<org>) — note: GitHub Packages still requires an
     authenticated nuget.config (a PAT), it is NOT consumable as a bare git URL.
Until then, consume via git submodule + ProjectReference (see above).
```

## Quick usage

```csharp
using Csilgen.Transport;

// RPC client over your own carrier (here, the in-memory loopback).
var carrier = new LoopbackFrameCarrier();
var client = new RpcClient(carrier, multiplexed: true);
RpcResponse resp = client.Call("Attestation", "deposit-claim", requestPayloadBytes);

// Server side: decode a request, dispatch, encode a reply.
var server = new RpcServer(carrier);
server.ServeOne(req => HandlerOutcome.Reply("DepositClaimResponse", replyPayloadBytes));
```
