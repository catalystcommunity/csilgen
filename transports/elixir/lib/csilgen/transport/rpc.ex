defmodule Csilgen.Transport.RPC do
  @moduledoc """
  CSIL-RPC transport — request/response/push envelopes — see csil-rpc-transport.md.

  Envelopes are structs; encode/decode are pure functions of binaries (no
  processes). A `null` field in a logical message is ABSENT on the wire (the key
  is omitted), and an empty response payload is still a PRESENT tag-24 byte string
  wrapping empty bytes — both are normative conventions enforced here.
  """

  alias Csilgen.Transport.{CBOR, Conventions, Status}

  defmodule Request do
    @moduledoc "A CSIL-RPC request (client → server)."
    @enforce_keys [:service, :op]
    defstruct [:service, :op, :id, :auth, payload: <<>>]

    @type t :: %__MODULE__{
            service: String.t(),
            op: String.t(),
            id: non_neg_integer() | nil,
            auth: String.t() | nil,
            payload: binary()
          }
  end

  defmodule Response do
    @moduledoc "A CSIL-RPC response (server → client)."
    @enforce_keys [:status]
    defstruct [:id, :status, :variant, :error, payload: <<>>]

    @type t :: %__MODULE__{
            id: non_neg_integer() | nil,
            status: integer(),
            variant: String.t() | nil,
            error: String.t() | nil,
            payload: binary()
          }
  end

  defmodule Push do
    @moduledoc "A CSIL-RPC server push (server → client) for `<-` operations."
    @enforce_keys [:service, :event]
    defstruct [:service, :event, payload: <<>>]

    @type t :: %__MODULE__{service: String.t(), event: String.t(), payload: binary()}
  end

  # --- Request --------------------------------------------------------------

  @doc "Builds a request with no correlation id and no per-request auth."
  @spec new_request(String.t(), String.t(), binary()) :: Request.t()
  def new_request(service, op, payload \\ <<>>) do
    %Request{service: service, op: op, payload: payload}
  end

  @spec encode_request(Request.t()) :: binary()
  def encode_request(%Request{} = r) do
    entries =
      [
        {"v", Conventions.version()},
        {"service", r.service},
        {"op", r.op},
        {"payload", Conventions.tag24(r.payload)}
      ]
      |> maybe_put("id", r.id)
      |> maybe_put("auth", r.auth)

    CBOR.encode({:map, entries})
  end

  @spec decode_request(binary()) :: {:ok, Request.t()} | {:error, term()}
  def decode_request(bytes) do
    with {:ok, map} <- Conventions.decode_envelope(bytes),
         {:ok, v} <- Conventions.get_int(map, "v"),
         :ok <- Conventions.check_version(v),
         {:ok, payload} <- Conventions.get_payload(map),
         {:ok, service} <- Conventions.get_text(map, "service"),
         {:ok, op} <- Conventions.get_text(map, "op") do
      {:ok,
       %Request{
         service: service,
         op: op,
         id: Conventions.get_uint_opt(map, "id"),
         auth: Conventions.get_text_opt(map, "auth"),
         payload: payload
       }}
    end
  end

  # --- Response -------------------------------------------------------------

  @doc "Builds a successful (status ok) typed reply."
  @spec new_response_ok(String.t(), binary()) :: Response.t()
  def new_response_ok(variant, payload) do
    %Response{status: Status.ok(), variant: variant, payload: payload}
  end

  @doc "Builds a transport-level failure (no typed payload)."
  @spec new_response_transport_error(integer(), String.t()) :: Response.t()
  def new_response_transport_error(status, message) do
    %Response{status: status, error: message, payload: <<>>}
  end

  @spec encode_response(Response.t()) :: binary()
  def encode_response(%Response{} = r) do
    entries =
      [
        {"v", Conventions.version()},
        {"status", r.status},
        {"payload", Conventions.tag24(r.payload)}
      ]
      |> maybe_put("id", r.id)
      |> maybe_put("variant", r.variant)
      |> maybe_put("error", r.error)

    CBOR.encode({:map, entries})
  end

  @spec decode_response(binary()) :: {:ok, Response.t()} | {:error, term()}
  def decode_response(bytes) do
    with {:ok, map} <- Conventions.decode_envelope(bytes),
         {:ok, v} <- Conventions.get_int(map, "v"),
         :ok <- Conventions.check_version(v),
         {:ok, payload} <- Conventions.get_payload(map),
         {:ok, status} <- Conventions.get_int(map, "status") do
      {:ok,
       %Response{
         id: Conventions.get_uint_opt(map, "id"),
         status: status,
         variant: Conventions.get_text_opt(map, "variant"),
         error: Conventions.get_text_opt(map, "error"),
         payload: payload
       }}
    end
  end

  @doc """
  Returns `{:error, {:status, name, code, message}}` for a non-ok response, or
  `:ok` when it carries a typed reply (status 0).
  """
  @spec as_transport_error(Response.t()) :: :ok | {:error, term()}
  def as_transport_error(%Response{status: status} = r) do
    if Status.ok?(status) do
      :ok
    else
      {:error, {:status, Status.name(status), status, r.error || ""}}
    end
  end

  # --- Push -----------------------------------------------------------------

  @spec new_push(String.t(), String.t(), binary()) :: Push.t()
  def new_push(service, event, payload \\ <<>>) do
    %Push{service: service, event: event, payload: payload}
  end

  @spec encode_push(Push.t()) :: binary()
  def encode_push(%Push{} = p) do
    CBOR.encode(
      {:map,
       [
         {"v", Conventions.version()},
         {"service", p.service},
         {"event", p.event},
         {"payload", Conventions.tag24(p.payload)}
       ]}
    )
  end

  @spec decode_push(binary()) :: {:ok, Push.t()} | {:error, term()}
  def decode_push(bytes) do
    with {:ok, map} <- Conventions.decode_envelope(bytes),
         {:ok, v} <- Conventions.get_int(map, "v"),
         :ok <- Conventions.check_version(v),
         {:ok, payload} <- Conventions.get_payload(map),
         {:ok, service} <- Conventions.get_text(map, "service"),
         {:ok, event} <- Conventions.get_text(map, "event") do
      {:ok, %Push{service: service, event: event, payload: payload}}
    end
  end

  # A `nil` optional field is ABSENT on the wire — the key is omitted, never encoded
  # as a present null.
  defp maybe_put(entries, _key, nil), do: entries
  defp maybe_put(entries, key, value), do: entries ++ [{key, value}]
end
