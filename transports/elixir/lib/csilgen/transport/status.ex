defmodule Csilgen.Transport.Status do
  @moduledoc """
  Transport-level status codes (conventions doc §4). Distinct from application
  errors, which ride inside the payload as a declared `/ ErrorType` arm.

  A status is carried on the wire as a plain integer code, so it is kept as an
  `integer()` in the envelope structs (equality is trivially by code, including
  host-defined extension codes ≥ 64 and unknown codes).
  """

  @ok 0
  @malformed_envelope 1
  @unknown_service_or_op 2
  @unauthenticated 3
  @forbidden 4
  @version_unsupported 5
  @internal 6
  @unavailable 7
  @deadline_exceeded 8

  @doc "Status code constant for `ok`."
  def ok, do: @ok
  def malformed_envelope, do: @malformed_envelope
  def unknown_service_or_op, do: @unknown_service_or_op
  def unauthenticated, do: @unauthenticated
  def forbidden, do: @forbidden
  def version_unsupported, do: @version_unsupported
  def internal, do: @internal
  def unavailable, do: @unavailable
  def deadline_exceeded, do: @deadline_exceeded

  @doc "Whether the status indicates a typed reply is present."
  @spec ok?(integer()) :: boolean()
  def ok?(@ok), do: true
  def ok?(_), do: false

  @doc "The registry name for a status code, or \"other\" for codes outside it."
  @spec name(integer()) :: String.t()
  def name(@ok), do: "ok"
  def name(@malformed_envelope), do: "malformed-envelope"
  def name(@unknown_service_or_op), do: "unknown-service-or-op"
  def name(@unauthenticated), do: "unauthenticated"
  def name(@forbidden), do: "forbidden"
  def name(@version_unsupported), do: "version-unsupported"
  def name(@internal), do: "internal"
  def name(@unavailable), do: "unavailable"
  def name(@deadline_exceeded), do: "deadline-exceeded"
  def name(_), do: "other"
end
