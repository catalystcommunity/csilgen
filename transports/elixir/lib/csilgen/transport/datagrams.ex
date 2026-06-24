defmodule Csilgen.Transport.Datagrams do
  @moduledoc """
  CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
  csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header
  profiles. A datagram channel is single-service, so datagrams carry no service
  ordinal.

  Per-profile integer widths come from the spec, not the JSON: the compact header
  uses `seq`=u16, `op_ord`=u8, `epoch`=u8; the cbor-array profile uses full CBOR
  uints.
  """

  # Imported before any use: Elixir `import` is lexically scoped from its point
  # forward, so the bitwise macros must be brought in above the encode/decode
  # clauses that rely on them.
  import Bitwise

  alias Csilgen.Transport.{CBOR, Conventions}

  # Conservative max datagram size safe across UDP/WebRTC/QUIC.
  @max_datagram_default 1200
  @compact_ver 1
  @flag_epoch 0x01

  @doc "The conservative max datagram size (envelope + payload)."
  @spec max_datagram_default() :: pos_integer()
  def max_datagram_default, do: @max_datagram_default

  defmodule Datagram do
    @moduledoc "A datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload]."
    @enforce_keys [:op_ord, :seq]
    defstruct [:op_ord, :seq, payload: <<>>]

    @type t :: %__MODULE__{
            op_ord: non_neg_integer(),
            seq: non_neg_integer(),
            payload: binary()
          }
  end

  defmodule CompactDatagram do
    @moduledoc """
    A datagram in the compact fixed-header profile. Header layout:
    `[ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8])` then the opaque body.
    """
    @enforce_keys [:op_ord, :seq]
    defstruct [:op_ord, :seq, :epoch, body: <<>>]

    @type t :: %__MODULE__{
            op_ord: non_neg_integer(),
            seq: non_neg_integer(),
            epoch: non_neg_integer() | nil,
            body: binary()
          }
  end

  @spec new_datagram(non_neg_integer(), non_neg_integer(), binary()) :: Datagram.t()
  def new_datagram(op_ord, seq, payload),
    do: %Datagram{op_ord: op_ord, seq: seq, payload: payload}

  @spec encode_datagram(Datagram.t()) :: binary()
  def encode_datagram(%Datagram{} = d) do
    CBOR.encode([Conventions.version(), d.op_ord, d.seq, Conventions.tag24(d.payload)])
  end

  @spec decode_datagram(binary()) :: {:ok, Datagram.t()} | {:error, term()}
  def decode_datagram(bytes) do
    with {:ok, arr} when is_list(arr) <- Conventions.decode_envelope(bytes) do
      case arr do
        [v, op_ord, seq, payload_v]
        when is_integer(v) and is_integer(op_ord) and is_integer(seq) ->
          with :ok <- Conventions.check_version(v),
               {:ok, payload} <- Conventions.untag24(payload_v) do
            {:ok, %Datagram{op_ord: op_ord, seq: seq, payload: payload}}
          end

        _ ->
          {:error, :bad_datagram}
      end
    end
  end

  @spec new_compact(non_neg_integer(), non_neg_integer(), binary()) :: CompactDatagram.t()
  def new_compact(op_ord, seq, body), do: %CompactDatagram{op_ord: op_ord, seq: seq, body: body}

  @doc "Sets the epoch byte (and the flag bit that signals its presence)."
  @spec with_epoch(CompactDatagram.t(), non_neg_integer()) :: CompactDatagram.t()
  def with_epoch(%CompactDatagram{} = d, epoch), do: %{d | epoch: epoch}

  @spec encode_compact(CompactDatagram.t()) :: binary()
  def encode_compact(%CompactDatagram{epoch: nil} = d) do
    flags_byte = @compact_ver <<< 4
    <<flags_byte, d.op_ord::8, d.seq::big-unsigned-integer-size(16), d.body::binary>>
  end

  def encode_compact(%CompactDatagram{} = d) do
    flags_byte = @compact_ver <<< 4 ||| @flag_epoch
    <<flags_byte, d.op_ord::8, d.seq::big-unsigned-integer-size(16), d.epoch::8, d.body::binary>>
  end

  @spec decode_compact(binary()) :: {:ok, CompactDatagram.t()} | {:error, term()}
  def decode_compact(<<vf::8, op_ord::8, seq::big-unsigned-integer-size(16), rest::binary>>) do
    ver = vf >>> 4
    flags = vf &&& 0x0F

    cond do
      ver != @compact_ver ->
        {:error, {:unsupported_version, ver}}

      (flags &&& @flag_epoch) != 0 ->
        case rest do
          <<epoch::8, body::binary>> ->
            {:ok, %CompactDatagram{op_ord: op_ord, seq: seq, epoch: epoch, body: body}}

          _ ->
            {:error, :missing_epoch_byte}
        end

      true ->
        {:ok, %CompactDatagram{op_ord: op_ord, seq: seq, body: rest}}
    end
  end

  def decode_compact(_), do: {:error, :short_compact_header}

  # --- Sequence tracking ----------------------------------------------------

  defmodule SeqTracker do
    @moduledoc """
    Tracks the last sequence/epoch per channel to classify arrivals for
    loss/reorder/restart detection. The transport detects; the app decides.
    Threaded immutably (no process state).
    """
    defstruct last_seq: nil, last_epoch: nil
    @type t :: %__MODULE__{last_seq: non_neg_integer() | nil, last_epoch: non_neg_integer() | nil}
  end

  @doc "A fresh sequence tracker."
  @spec new_tracker() :: SeqTracker.t()
  def new_tracker, do: %SeqTracker{}

  @doc """
  Classifies an arriving `(seq, epoch)` and returns the updated tracker.

  Outcomes: `:first`, `{:advanced, gap}`, `:late_or_duplicate`, `:restart`. A
  restart fires only when a *prior* epoch existed and changed (no-epoch → first
  epoch is NOT a restart). An unsequenced datagram (seq 0) is `{:advanced, 0}`.
  """
  @spec observe(SeqTracker.t(), non_neg_integer(), non_neg_integer() | nil) ::
          {:first | {:advanced, non_neg_integer()} | :late_or_duplicate | :restart,
           SeqTracker.t()}
  def observe(%SeqTracker{} = t, seq, epoch) do
    cond do
      epoch != t.last_epoch and t.last_epoch != nil ->
        {:restart, %{t | last_epoch: epoch, last_seq: seq}}

      true ->
        t = %{t | last_epoch: epoch}
        classify_seq(t, seq)
    end
  end

  # seq 0 marks an unsequenced datagram: it carries no ordering information, so it
  # is never late or duplicate. Report a zero-gap advance and leave the running
  # sequence untouched.
  defp classify_seq(%SeqTracker{} = t, 0), do: {{:advanced, 0}, t}

  defp classify_seq(%SeqTracker{last_seq: nil} = t, seq),
    do: {:first, %{t | last_seq: seq}}

  defp classify_seq(%SeqTracker{last_seq: last} = t, seq) when seq > last,
    do: {{:advanced, seq - last - 1}, %{t | last_seq: seq}}

  defp classify_seq(%SeqTracker{} = t, _seq), do: {:late_or_duplicate, t}
end
