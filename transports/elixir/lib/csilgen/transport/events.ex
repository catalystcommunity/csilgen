defmodule Csilgen.Transport.Events do
  @moduledoc """
  CSIL-Events transport — typed bidirectional event streams — see
  csil-events-transport.md. Verbose (text-keyed) and compact (positional)
  profiles, plus the control-plane (service ordinal 0) lifecycle payloads.
  """

  alias Csilgen.Transport.{CBOR, Conventions}

  defmodule Event do
    @moduledoc """
    One typed event flowing in either direction. Identified by service+operation
    (verbose) or their ordinals (compact); carries an optional correlation id.
    """
    defstruct [:service, :service_ord, :event, :op_ord, :id, payload: <<>>]

    @type t :: %__MODULE__{
            service: String.t() | nil,
            service_ord: non_neg_integer() | nil,
            event: String.t() | nil,
            op_ord: non_neg_integer() | nil,
            id: non_neg_integer() | nil,
            payload: binary()
          }
  end

  @doc "Builds a verbose event by name."
  @spec new_verbose(String.t() | nil, String.t(), binary()) :: Event.t()
  def new_verbose(service, event, payload) do
    %Event{service: service, event: event, payload: payload}
  end

  @doc "Builds a compact event by ordinals."
  @spec new_compact(non_neg_integer(), non_neg_integer(), binary()) :: Event.t()
  def new_compact(service_ord, op_ord, payload) do
    %Event{service_ord: service_ord, op_ord: op_ord, payload: payload}
  end

  @doc "Parses a wire profile name."
  @spec parse_profile(String.t()) :: {:ok, :verbose | :compact} | :error
  def parse_profile("verbose"), do: {:ok, :verbose}
  def parse_profile("compact"), do: {:ok, :compact}
  def parse_profile(_), do: :error

  @spec encode_event(Event.t(), :verbose | :compact) :: binary()
  def encode_event(%Event{} = e, :verbose) do
    entries =
      [{"event", e.event}, {"payload", Conventions.tag24(e.payload)}]
      |> maybe_put("service", e.service)
      |> maybe_put("id", e.id)

    CBOR.encode({:map, entries})
  end

  def encode_event(%Event{} = e, :compact) do
    arr = [e.service_ord, e.op_ord]
    arr = if e.id != nil, do: arr ++ [e.id], else: arr
    CBOR.encode(arr ++ [Conventions.tag24(e.payload)])
  end

  @spec decode_event(binary(), :verbose | :compact) :: {:ok, Event.t()} | {:error, term()}
  def decode_event(bytes, :verbose) do
    with {:ok, map} <- Conventions.decode_envelope(bytes),
         {:ok, payload} <- Conventions.get_payload(map),
         {:ok, event} <- Conventions.get_text(map, "event") do
      {:ok,
       %Event{
         service: Conventions.get_text_opt(map, "service"),
         event: event,
         id: Conventions.get_uint_opt(map, "id"),
         payload: payload
       }}
    end
  end

  def decode_event(bytes, :compact) do
    with {:ok, arr} when is_list(arr) <- Conventions.decode_envelope(bytes) do
      # 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
      case arr do
        [so, oo, payload_v] ->
          build_compact(so, oo, nil, payload_v)

        [so, oo, id_v, payload_v] ->
          build_compact(so, oo, id_v, payload_v)

        _ ->
          {:error, :bad_compact_arity}
      end
    end
  end

  defp build_compact(so, oo, id_v, payload_v)
       when is_integer(so) and is_integer(oo) do
    with {:ok, payload} <- Conventions.untag24(payload_v) do
      {:ok, %Event{service_ord: so, op_ord: oo, id: id_v, payload: payload}}
    end
  end

  defp build_compact(_so, _oo, _id, _payload), do: {:error, :non_integer_ordinal}

  # --- Control plane --------------------------------------------------------

  defmodule Hello do
    @moduledoc "The `$hello` payload offered by the connection initiator."
    @enforce_keys [:versions, :profiles]
    defstruct [:versions, :profiles, :service, :auth]

    @type t :: %__MODULE__{
            versions: [non_neg_integer()],
            profiles: [String.t()],
            service: String.t() | nil,
            auth: String.t() | nil
          }
  end

  defmodule HelloAck do
    @moduledoc "The `$hello-ack` payload returned by the peer."
    @enforce_keys [:v, :profile]
    defstruct [:v, :profile, :session]
    @type t :: %__MODULE__{v: non_neg_integer(), profile: String.t(), session: String.t() | nil}
  end

  defmodule Heartbeat do
    @moduledoc "A `$ping`/`$pong` heartbeat payload."
    @enforce_keys [:nonce]
    defstruct [:nonce, :at]
    @type t :: %__MODULE__{nonce: non_neg_integer(), at: non_neg_integer() | nil}
  end

  defmodule Close do
    @moduledoc "A `$close` payload."
    @enforce_keys [:status]
    defstruct [:status, :reason]
    @type t :: %__MODULE__{status: integer(), reason: String.t() | nil}
  end

  @spec encode_hello(Hello.t()) :: binary()
  def encode_hello(%Hello{} = h) do
    entries =
      [{"versions", h.versions}, {"profiles", h.profiles}]
      |> maybe_put("service", h.service)
      |> maybe_put("auth", h.auth)

    CBOR.encode({:map, entries})
  end

  @spec encode_hello_ack(HelloAck.t()) :: binary()
  def encode_hello_ack(%HelloAck{} = a) do
    entries =
      [{"v", a.v}, {"profile", a.profile}]
      |> maybe_put("session", a.session)

    CBOR.encode({:map, entries})
  end

  @spec encode_heartbeat(Heartbeat.t()) :: binary()
  def encode_heartbeat(%Heartbeat{} = h) do
    entries = [{"nonce", h.nonce}] |> maybe_put("at", h.at)
    CBOR.encode({:map, entries})
  end

  @spec encode_close(Close.t()) :: binary()
  def encode_close(%Close{} = c) do
    entries = [{"status", c.status}] |> maybe_put("reason", c.reason)
    CBOR.encode({:map, entries})
  end

  defp maybe_put(entries, _key, nil), do: entries
  defp maybe_put(entries, key, value), do: entries ++ [{key, value}]
end
