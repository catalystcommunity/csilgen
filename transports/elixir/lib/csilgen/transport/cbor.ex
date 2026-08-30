defmodule Csilgen.Transport.CBOR do
  @moduledoc """
  Minimal canonical CBOR codec (RFC 8949), hand-written and dependency-free.

  It supports exactly what the CSIL envelopes need — unsigned ints, negative ints,
  text strings, byte strings, arrays, maps, and tags — and nothing else. Maps use
  core deterministic encoding: entries are sorted by the bytewise lexicographic
  order of their *encoded* keys, which for Elixir binaries is the default `<=/2`,
  so the bytes match the conformance vectors regardless of insertion order.

  In-memory model (the items encode/decode produce and consume):

    * unsigned/negative int  → Elixir `integer()` (bignum: a CBOR uint64 round-trips
      with no overflow guard, and a negative-int decode needs no signed-floor guard)
    * text string            → bare `binary()`
    * byte string            → `{:bytes, binary()}` (tagged so the encoder never
      emits a tstr where a bstr is required — the bstr/tstr ambiguity trap)
    * array                  → `list()`
    * map                    → `{:map, [{key, value}]}` on encode; `%{}` on decode
    * tag                    → `{:tag, non_neg_integer(), value}`
  """

  @doc "Encodes one value to canonical CBOR bytes."
  @spec encode(term()) :: binary()
  def encode(value), do: value |> encode_iodata() |> IO.iodata_to_binary()

  # An envelope is a single self-contained item; trailing bytes are a malformed
  # frame (conventions doc §1), rejected here rather than silently ignored.
  @doc "Decodes one complete CBOR item, rejecting any trailing bytes."
  @spec decode(binary()) :: {:ok, term()} | {:error, atom()}
  def decode(bin) when is_binary(bin) do
    with {:ok, value, rest} <- decode_value(bin, 0) do
      if rest == <<>>, do: {:ok, value}, else: {:error, :trailing_bytes}
    end
  end

  # --- encode ---------------------------------------------------------------

  defp encode_iodata(n) when is_integer(n) and n >= 0, do: head(0, n)
  # CBOR negative ints encode -1-n; the argument is the magnitude minus one.
  defp encode_iodata(n) when is_integer(n) and n < 0, do: head(1, -1 - n)
  defp encode_iodata({:bytes, b}) when is_binary(b), do: [head(2, byte_size(b)), b]
  # UTF-8 is the native binary encoding, so byte_size/1 gives the wire length.
  defp encode_iodata(s) when is_binary(s), do: [head(3, byte_size(s)), s]

  defp encode_iodata(list) when is_list(list) do
    [head(4, length(list)) | Enum.map(list, &encode_iodata/1)]
  end

  defp encode_iodata({:map, entries}) when is_list(entries) do
    body =
      entries
      |> Enum.map(fn {k, v} -> {IO.iodata_to_binary(encode_iodata(k)), encode_iodata(v)} end)
      # Default <=/2 on binaries is bytewise lexicographic, shorter-prefix-first —
      # exactly RFC 8949 §4.2.1 once length is folded into the head byte.
      |> Enum.sort_by(fn {kb, _} -> kb end)
      |> Enum.flat_map(fn {kb, ve} -> [kb, ve] end)

    [head(5, length(entries)) | body]
  end

  defp encode_iodata({:tag, num, value}) when is_integer(num) and num >= 0 do
    [head(6, num), encode_iodata(value)]
  end

  # `<<n::16/32/64>>` default to big-endian unsigned — CBOR's network order — so no
  # qualifier is required, though the explicit form is kept for self-documentation.
  defp head(major, n) when n < 24, do: <<major::3, n::5>>
  defp head(major, n) when n < 0x100, do: <<major::3, 24::5, n::big-unsigned-integer-size(8)>>
  defp head(major, n) when n < 0x10000, do: <<major::3, 25::5, n::big-unsigned-integer-size(16)>>

  defp head(major, n) when n < 0x100000000,
    do: <<major::3, 26::5, n::big-unsigned-integer-size(32)>>

  defp head(major, n), do: <<major::3, 27::5, n::big-unsigned-integer-size(64)>>

  # --- decode ---------------------------------------------------------------

  defp decode_value(_bin, depth) when depth > 64, do: {:error, :nesting_limit_exceeded}

  defp decode_value(<<major::3, info::5, rest::binary>>, depth) do
    with {:ok, arg, rest} <- read_arg(info, rest) do
      decode_item(major, arg, rest, depth)
    end
  end

  defp decode_value(<<>>, _depth), do: {:error, :empty_input}

  defp read_arg(info, rest) when info < 24, do: {:ok, info, rest}
  defp read_arg(24, <<n::8, rest::binary>>), do: {:ok, n, rest}
  defp read_arg(25, <<n::16, rest::binary>>), do: {:ok, n, rest}
  defp read_arg(26, <<n::32, rest::binary>>), do: {:ok, n, rest}
  defp read_arg(27, <<n::64, rest::binary>>), do: {:ok, n, rest}
  defp read_arg(info, _rest) when info in 24..27, do: {:error, :truncated_argument}
  # 28..30 (reserved) and 31 (indefinite) are forbidden in CSIL envelopes.
  defp read_arg(_info, _rest), do: {:error, :indefinite_or_reserved}

  defp decode_item(0, arg, rest, _depth), do: {:ok, arg, rest}
  # Bignum integers make -1-arg exact with no signed-64 floor guard needed.
  defp decode_item(1, arg, rest, _depth), do: {:ok, -1 - arg, rest}

  defp decode_item(2, arg, rest, _depth) do
    # Pattern-matching a fixed-size slice never over-allocates on a forged length:
    # it only matches when the bytes are actually present.
    case rest do
      <<b::binary-size(^arg), rest2::binary>> -> {:ok, {:bytes, b}, rest2}
      _ -> {:error, :truncated_byte_string}
    end
  end

  defp decode_item(3, arg, rest, _depth) do
    case rest do
      <<s::binary-size(^arg), rest2::binary>> ->
        if String.valid?(s), do: {:ok, s, rest2}, else: {:error, :invalid_utf8}
      _ -> {:error, :truncated_text_string}
    end
  end

  defp decode_item(4, arg, rest, depth) when arg <= byte_size(rest),
    do: decode_seq(arg, rest, [], depth + 1)

  defp decode_item(4, _arg, _rest, _depth), do: {:error, :array_length_exceeds_remaining_input}

  defp decode_item(5, arg, rest, depth) when arg <= byte_size(rest),
    do: decode_map(arg, rest, [], depth + 1)

  defp decode_item(5, _arg, _rest, _depth), do: {:error, :map_length_exceeds_remaining_input}

  defp decode_item(6, arg, rest, depth) do
    with {:ok, value, rest2} <- decode_value(rest, depth + 1), do: {:ok, {:tag, arg, value}, rest2}
  end

  defp decode_item(7, _arg, _rest, _depth), do: {:error, :unsupported_simple_or_float}

  defp decode_seq(0, rest, acc, _depth), do: {:ok, Enum.reverse(acc), rest}

  defp decode_seq(n, rest, acc, depth) do
    with {:ok, item, rest2} <- decode_value(rest, depth) do
      decode_seq(n - 1, rest2, [item | acc], depth)
    end
  end

  defp decode_map(0, rest, acc, _depth), do: {:ok, Map.new(acc), rest}

  defp decode_map(n, rest, acc, depth) do
    with {:ok, key, rest2} <- decode_value(rest, depth),
         {:ok, value, rest3} <- decode_value(rest2, depth) do
      decode_map(n - 1, rest3, [{key, value} | acc], depth)
    end
  end
end
