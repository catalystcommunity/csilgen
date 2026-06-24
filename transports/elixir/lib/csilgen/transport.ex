defmodule Csilgen.Transport do
  @moduledoc """
  CSIL transport library — a matched client+server for the three CSIL transports
  (RPC, Events, Datagrams) over a shared, hand-rolled canonical-CBOR codec with
  zero external dependencies.

  Everything here is synchronous and pure: `encode`/`decode` are functions of
  binaries, never processes (no async, ever — the house rule). The host owns the
  I/O loop and supplies a carrier via `Csilgen.Transport.Carrier`.

  Submodules:

    * `Csilgen.Transport.CBOR` — canonical CBOR encode/decode
    * `Csilgen.Transport.Conventions` — version, tag-24 payloads, status, guards
    * `Csilgen.Transport.Status` — transport status registry
    * `Csilgen.Transport.Carrier` — bring-your-own-carrier seam + loopback
    * `Csilgen.Transport.RPC` — request/response/push envelopes
    * `Csilgen.Transport.Events` — verbose/compact events + control plane
    * `Csilgen.Transport.Datagrams` — cbor-array + compact-header datagrams
  """

  @version "0.1.0"

  @doc "The library version."
  @spec version() :: String.t()
  def version, do: @version
end
