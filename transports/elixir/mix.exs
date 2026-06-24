defmodule Csilgen.Transport.MixProject do
  use Mix.Project

  @version "0.1.0"

  def project do
    [
      app: :csilgen_transport,
      version: @version,
      # The native JSON module used by the conformance tests is Elixir 1.18+/OTP
      # 27+; nothing in the library proper needs a newer feature.
      elixir: "~> 1.18",
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      description: description(),
      package: package()
    ]
  end

  def application do
    # No supervision tree: the library is pure functions plus an injected carrier;
    # the host owns the I/O loop.
    [extra_applications: [:logger]]
  end

  # Zero external dependencies: the canonical CBOR codec is hand-rolled, and the
  # conformance tests use the OTP-native JSON module and stdlib Base.
  defp deps, do: []

  defp description do
    "Synchronous, dependency-free CSIL transport library (RPC, Events, Datagrams) " <>
      "over a hand-rolled canonical CBOR codec."
  end

  # TODO: Hex publishing is deferred — no packages are published yet. To publish
  # later, fill in `links` and run `mix hex.publish`. Until then the supported
  # consumption path is the git+sparse dep documented in README.md.
  defp package do
    [
      licenses: ["Apache-2.0"],
      links: %{"Source" => "https://github.com/catalystcommunity/csilgen"},
      files: ~w(lib mix.exs README.md .formatter.exs)
    ]
  end
end
