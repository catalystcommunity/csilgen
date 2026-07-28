defmodule Csilgen.Transport.Carrier do
  @moduledoc """
  The bring-your-own-carrier seam (conventions doc §7).

  The library owns envelope codecs, framing, and lifecycle; the *carrier* — the
  byte/datagram transport — is injected by the host. A host supplies QUIC,
  WebRTC, a WebSocket, a platform media stack, or anything else by implementing
  this behaviour. The library never spawns the carrier: the `carrier` argument is
  an opaque term the host owns (a socket ref, a struct, a pid), and every call is
  synchronous, so the host keeps ownership of the I/O loop (no async, no
  processes on the codec path).

  A carrier call returns the (possibly updated) carrier alongside its result, so
  an immutable in-memory carrier can be threaded functionally — the
  `Csilgen.Transport.Carrier.Loopback` built-in does exactly that. A
  socket-backed carrier whose state lives in the host simply returns the same
  term it was given.
  """

  @typedoc "An opaque, host-owned carrier value."
  @type carrier :: term()

  @doc "Sends one delimited frame, returning the updated carrier."
  @callback send_frame(carrier(), frame :: binary()) :: {:ok, carrier()} | {:error, term()}

  @doc """
  Receives the next frame. `:closed` signals a clean end of stream before any
  frame byte; `{:ok, frame, carrier}` carries the frame and the updated carrier.
  """
  @callback recv_frame(carrier()) :: {:ok, binary(), carrier()} | :closed | {:error, term()}

  import Csilgen.Transport.Conventions, only: [is_valid_max_frame: 1]

  @doc "The default 16 MiB max-frame guard, shared with the conventions layer."
  @spec max_frame_default() :: pos_integer()
  def max_frame_default, do: Csilgen.Transport.Conventions.max_frame_default()

  @doc "The largest max-frame guard a host may configure, shared with the conventions layer."
  @spec max_frame_limit() :: pos_integer()
  def max_frame_limit, do: Csilgen.Transport.Conventions.max_frame_limit()

  @doc """
  Wraps a frame in CSIL stream framing: a 4-byte big-endian length prefix. The
  guard is enforced before producing the prefix so an oversized frame is rejected
  rather than emitted.
  """
  @spec frame_length_prefixed(binary(), pos_integer()) ::
          {:ok, binary()}
          | {:error, {:frame_too_large, non_neg_integer(), pos_integer()}}
          | {:error, {:invalid_max_frame, term(), pos_integer()}}
  def frame_length_prefixed(frame, max \\ nil)

  def frame_length_prefixed(frame, nil), do: frame_length_prefixed(frame, max_frame_default())

  # A limit outside the valid range is rejected here rather than silently framing
  # against an unusable guard.
  def frame_length_prefixed(_frame, max) when not is_valid_max_frame(max) do
    {:error, {:invalid_max_frame, max, max_frame_limit()}}
  end

  def frame_length_prefixed(frame, max) when is_binary(frame) and byte_size(frame) > max do
    {:error, {:frame_too_large, byte_size(frame), max}}
  end

  def frame_length_prefixed(frame, _max) when is_binary(frame) do
    {:ok, <<byte_size(frame)::big-unsigned-integer-size(32), frame::binary>>}
  end

  @doc """
  Reads one length-prefixed frame from a buffer, enforcing the 16 MiB guard
  *before* slicing so a forged length can never drive a giant allocation. Returns
  the frame and the remaining buffer; `:incomplete` when the buffer holds less
  than a full frame.
  """
  @spec read_length_prefixed(binary(), pos_integer()) ::
          {:ok, binary(), binary()}
          | :incomplete
          | {:error, {:frame_too_large, non_neg_integer(), pos_integer()}}
          | {:error, {:invalid_max_frame, term(), pos_integer()}}
  def read_length_prefixed(buffer, max \\ nil)

  def read_length_prefixed(buffer, nil), do: read_length_prefixed(buffer, max_frame_default())

  # A limit outside the valid range is rejected before any length is even read, so
  # a misconfigured guard can never admit an oversized allocation.
  def read_length_prefixed(_buffer, max) when not is_valid_max_frame(max) do
    {:error, {:invalid_max_frame, max, max_frame_limit()}}
  end

  # The length is read as an unsigned 32-bit value and checked against the guard
  # before the body slice is even attempted.
  def read_length_prefixed(<<len::big-unsigned-integer-size(32), _rest::binary>>, max)
      when len > max do
    {:error, {:frame_too_large, len, max}}
  end

  def read_length_prefixed(<<len::big-unsigned-integer-size(32), rest::binary>>, _max) do
    case rest do
      <<frame::binary-size(^len), tail::binary>> -> {:ok, frame, tail}
      _ -> :incomplete
    end
  end

  def read_length_prefixed(_buffer, _max), do: :incomplete
end

defmodule Csilgen.Transport.Carrier.Loopback do
  @moduledoc """
  An in-memory `Csilgen.Transport.Carrier` for tests and for driving the codec
  without a socket. Pid-free: state is an immutable struct threaded through every
  call, so it stays on the pure codec path (no GenServer).

  `send_frame/2` appends to `outbound`; `recv_frame/1` pops from `inbound`. Use
  `push_inbound/2` to stage frames a later `recv_frame/1` will return, and
  `take_outbound/1` to drain what was sent.
  """

  @behaviour Csilgen.Transport.Carrier

  defstruct inbound: [], outbound: []

  @type t :: %__MODULE__{inbound: [binary()], outbound: [binary()]}

  @doc "A fresh loopback carrier."
  @spec new() :: t()
  def new, do: %__MODULE__{}

  @doc "Stages a frame that a later `recv_frame/1` will return."
  @spec push_inbound(t(), binary()) :: t()
  def push_inbound(%__MODULE__{} = c, frame) when is_binary(frame) do
    %{c | inbound: c.inbound ++ [frame]}
  end

  @doc "Removes and returns the oldest sent frame, or `:empty`."
  @spec take_outbound(t()) :: {:ok, binary(), t()} | :empty
  def take_outbound(%__MODULE__{outbound: []}), do: :empty

  def take_outbound(%__MODULE__{outbound: [f | rest]} = c) do
    {:ok, f, %{c | outbound: rest}}
  end

  @impl true
  def send_frame(%__MODULE__{} = c, frame) when is_binary(frame) do
    {:ok, %{c | outbound: c.outbound ++ [frame]}}
  end

  @impl true
  def recv_frame(%__MODULE__{inbound: []}), do: :closed

  def recv_frame(%__MODULE__{inbound: [f | rest]} = c) do
    {:ok, f, %{c | inbound: rest}}
  end
end
