defmodule Csilgen.Transport.Conventions do
  @moduledoc """
  Conventions shared by every CSIL transport — see csil-transport-conventions.md.

  Owns the version constant, the transport status registry, tag-24 payload
  wrap/unwrap, the max-frame guard, and the canonical-CBOR map accessors the
  envelopes build on so their bytes match the conformance vectors.
  """

  alias Csilgen.Transport.CBOR

  # A new value is minted only for a breaking change to envelope layout/semantics.
  @version 1
  # CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949 §3.4.5.1).
  @tag_encoded_cbor 24
  # Reserved service ordinal for the transport control plane (Events lifecycle).
  @control_service_ord 0
  # Default max encoded envelope size (16 MiB). A carrier rejects anything larger
  # before allocating for it.
  @max_frame_default 16 * 1024 * 1024

  @doc "The current transport version."
  @spec version() :: non_neg_integer()
  def version, do: @version

  @doc "The tag-24 (encoded-cbor) number."
  @spec tag_encoded_cbor() :: non_neg_integer()
  def tag_encoded_cbor, do: @tag_encoded_cbor

  @doc "The control-plane service ordinal (0)."
  @spec control_service_ord() :: non_neg_integer()
  def control_service_ord, do: @control_service_ord

  @doc "The default 16 MiB max-frame guard."
  @spec max_frame_default() :: pos_integer()
  def max_frame_default, do: @max_frame_default

  @doc "Wraps opaque payload bytes (themselves a CBOR item) in tag 24."
  @spec tag24(binary()) :: {:tag, non_neg_integer(), {:bytes, binary()}}
  def tag24(payload) when is_binary(payload), do: {:tag, @tag_encoded_cbor, {:bytes, payload}}

  @doc "Extracts the opaque payload bytes from a tag-24 value."
  @spec untag24(term()) :: {:ok, binary()} | {:error, atom()}
  def untag24({:tag, @tag_encoded_cbor, {:bytes, bytes}}) when is_binary(bytes), do: {:ok, bytes}
  def untag24(_other), do: {:error, :expected_tag24_payload}

  @doc "Verifies a decoded envelope's version field."
  @spec check_version(non_neg_integer()) ::
          :ok | {:error, {:unsupported_version, non_neg_integer()}}
  def check_version(@version), do: :ok
  def check_version(v), do: {:error, {:unsupported_version, v}}

  # --- canonical-CBOR map accessors (over a decoded %{} with binary keys) -----
  #
  # Wire map keys are kept as **binaries**, never atomized: an attacker spraying
  # distinct keys would otherwise exhaust the non-GC'd atom table and crash the VM.

  @doc "Looks up a text key in a decoded CBOR map."
  @spec map_get(term(), binary()) :: {:ok, term()} | :error
  def map_get(map, key) when is_map(map) and is_binary(key) do
    case Map.fetch(map, key) do
      {:ok, v} -> {:ok, v}
      :error -> :error
    end
  end

  def map_get(_map, _key), do: :error

  @doc "Reads a required text field."
  @spec get_text(term(), binary()) :: {:ok, binary()} | {:error, atom()}
  def get_text(map, key) do
    case map_get(map, key) do
      {:ok, v} when is_binary(v) -> {:ok, v}
      _ -> {:error, :missing_or_non_text}
    end
  end

  @doc "Reads an optional text field, returning nil when absent."
  @spec get_text_opt(term(), binary()) :: binary() | nil
  def get_text_opt(map, key) do
    case map_get(map, key) do
      {:ok, v} when is_binary(v) -> v
      _ -> nil
    end
  end

  @doc "Reads a required integer field."
  @spec get_int(term(), binary()) :: {:ok, integer()} | {:error, atom()}
  def get_int(map, key) do
    case map_get(map, key) do
      {:ok, v} when is_integer(v) -> {:ok, v}
      _ -> {:error, :missing_or_non_integer}
    end
  end

  @doc "Reads an optional non-negative integer field, returning nil when absent."
  @spec get_uint_opt(term(), binary()) :: non_neg_integer() | nil
  def get_uint_opt(map, key) do
    case map_get(map, key) do
      {:ok, v} when is_integer(v) and v >= 0 -> v
      _ -> nil
    end
  end

  @doc "Reads the required tag-24 payload field, returning its inner bytes."
  @spec get_payload(term()) :: {:ok, binary()} | {:error, atom()}
  def get_payload(map) do
    case map_get(map, "payload") do
      {:ok, v} -> untag24(v)
      :error -> {:error, :missing_payload}
    end
  end

  @doc "Decodes a complete envelope to a CBOR value (rejecting trailing bytes)."
  @spec decode_envelope(binary()) :: {:ok, term()} | {:error, atom()}
  def decode_envelope(bytes), do: CBOR.decode(bytes)
end
