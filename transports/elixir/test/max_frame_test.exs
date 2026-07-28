defmodule Csilgen.Transport.MaxFrameTest do
  @moduledoc """
  The configurable max-frame guard (conventions doc section 5): a host sets the
  limit up or down where the framing is configured, the limit applies to outbound
  frames and inbound length prefixes alike, an oversized prefix is rejected before
  its body is sliced, and an invalid limit is rejected rather than silently framing
  against an unusable guard.

  Elixir has no stateful stream carrier (a host owns its own `:gen_tcp` socket and
  buffer), so the knob is the `max` argument on `frame_length_prefixed/2` and
  `read_length_prefixed/2`.
  """

  use ExUnit.Case, async: true

  import Bitwise, only: [{:<<<, 2}]

  alias Csilgen.Transport.{Carrier, Conventions}

  @default Conventions.max_frame_default()
  @limit Conventions.max_frame_limit()

  test "default limit accepts a frame below it" do
    body = :binary.copy(<<0xAB>>, 1024)
    assert {:ok, framed} = Carrier.frame_length_prefixed(body)
    assert byte_size(framed) == 4 + byte_size(body)
    assert {:ok, ^body, <<>>} = Carrier.read_length_prefixed(framed)
  end

  test "default limit rejects a frame above it" do
    body = :binary.copy(<<0>>, @default + 1)
    assert {:error, {:frame_too_large, got, @default}} = Carrier.frame_length_prefixed(body)
    assert got == @default + 1
  end

  test "a larger custom limit accepts what the default rejects" do
    body = :binary.copy(<<0>>, @default + 1)
    raised = @default + 4096
    assert {:ok, framed} = Carrier.frame_length_prefixed(body, raised)
    assert {:ok, ^body, <<>>} = Carrier.read_length_prefixed(framed, raised)
  end

  test "a smaller custom limit rejects what the default accepts" do
    body = :binary.copy(<<0xCD>>, 1024)
    assert {:error, {:frame_too_large, 1024, 64}} = Carrier.frame_length_prefixed(body, 64)
  end

  test "an oversized incoming length is rejected before its body is sliced" do
    # Only the 4-byte prefix, no body at all. The guard must fire on the claim
    # itself rather than reporting :incomplete and waiting for 4 GiB.
    prefix = <<0xFFFFFFFF::big-unsigned-integer-size(32)>>
    assert {:error, {:frame_too_large, 4_294_967_295, 4096}} =
             Carrier.read_length_prefixed(prefix, 4096)
  end

  test "invalid limits are rejected" do
    for limit <- [0, -1, -4096, @limit + 1, 1 <<< 40, :not_an_int, 1.5] do
      assert {:error, {:invalid_max_frame, ^limit, @limit}} =
               Carrier.frame_length_prefixed(<<1>>, limit),
             "framing limit #{inspect(limit)} must be rejected"

      assert {:error, {:invalid_max_frame, ^limit, @limit}} =
               Carrier.read_length_prefixed(<<0, 0, 0, 1, 9>>, limit),
             "reading limit #{inspect(limit)} must be rejected"
    end
  end

  test "boundary limits are accepted" do
    for limit <- [1, @default, @limit] do
      assert {:ok, _} = Carrier.frame_length_prefixed(<<9>>, limit)
    end
  end
end
