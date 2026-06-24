defmodule Csilgen.Transport.RoundtripTest do
  @moduledoc """
  Example/property round-trips for the codec, carrier, and sequence tracker that
  the byte-exact conformance vectors don't directly exercise.
  """

  use ExUnit.Case, async: true

  alias Csilgen.Transport.{Carrier, CBOR, Conventions, Datagrams}
  alias Csilgen.Transport.Carrier.Loopback

  describe "cbor canonical codec" do
    test "integers use the shortest head form at each width boundary" do
      assert CBOR.encode(0) == <<0x00>>
      assert CBOR.encode(23) == <<0x17>>
      assert CBOR.encode(24) == <<0x18, 0x18>>
      assert CBOR.encode(255) == <<0x18, 0xFF>>
      assert CBOR.encode(256) == <<0x19, 0x01, 0x00>>
      assert CBOR.encode(65_535) == <<0x19, 0xFF, 0xFF>>
      assert CBOR.encode(65_536) == <<0x1A, 0x00, 0x01, 0x00, 0x00>>
      assert CBOR.encode(4_294_967_296) == <<0x1B, 0, 0, 0, 1, 0, 0, 0, 0>>
    end

    test "a uint64 max round-trips with no overflow guard (bignum)" do
      n = 0xFFFFFFFFFFFFFFFF
      assert {:ok, ^n} = CBOR.decode(CBOR.encode(n))
    end

    test "negative ints follow the -1-n magnitude rule" do
      assert CBOR.encode(-1) == <<0x20>>
      assert CBOR.encode(-24) == <<0x37>>
      assert CBOR.encode(-25) == <<0x38, 0x18>>
      assert {:ok, -25} = CBOR.decode(<<0x38, 0x18>>)
    end

    test "byte strings and text strings stay distinct across a round-trip" do
      assert CBOR.encode({:bytes, <<1, 2, 3>>}) == <<0x43, 1, 2, 3>>
      assert {:ok, {:bytes, <<1, 2, 3>>}} = CBOR.decode(<<0x43, 1, 2, 3>>)
      assert CBOR.encode("abc") == <<0x63, ?a, ?b, ?c>>
      assert {:ok, "abc"} = CBOR.decode(<<0x63, ?a, ?b, ?c>>)
    end

    test "map entries are emitted in canonical (encoded-key-bytes) order" do
      # Inserted out of order; the encoder must sort to v < id (shorter/lower first).
      bytes = CBOR.encode({:map, [{"id", 7}, {"v", 1}]})
      assert bytes == <<0xA2, 0x61, ?v, 0x01, 0x62, ?i, ?d, 0x07>>
      assert {:ok, %{"v" => 1, "id" => 7}} = CBOR.decode(bytes)
    end

    test "trailing bytes after one item are rejected" do
      assert {:error, :trailing_bytes} = CBOR.decode(<<0x00, 0x00>>)
    end

    test "indefinite-length and reserved additional-info are rejected" do
      assert {:error, :indefinite_or_reserved} = CBOR.decode(<<0x5F>>)
      assert {:error, :indefinite_or_reserved} = CBOR.decode(<<0x1C>>)
    end

    test "tag-24 payload wrap/unwrap round-trips" do
      wrapped = Conventions.tag24(<<0xA0>>)
      assert {:ok, {:tag, 24, {:bytes, <<0xA0>>}}} = CBOR.decode(CBOR.encode(wrapped))
      assert {:ok, <<0xA0>>} = Conventions.untag24({:tag, 24, {:bytes, <<0xA0>>}})
    end
  end

  describe "loopback carrier" do
    test "sent frames are taken back in FIFO order, threaded immutably" do
      c = Loopback.new()
      {:ok, c} = Loopback.send_frame(c, "a")
      {:ok, c} = Loopback.send_frame(c, "b")
      assert {:ok, "a", c} = Loopback.take_outbound(c)
      assert {:ok, "b", c} = Loopback.take_outbound(c)
      assert :empty = Loopback.take_outbound(c)
    end

    test "staged inbound frames are received then the carrier closes" do
      c = Loopback.new() |> Loopback.push_inbound("x") |> Loopback.push_inbound("y")
      assert {:ok, "x", c} = Loopback.recv_frame(c)
      assert {:ok, "y", c} = Loopback.recv_frame(c)
      assert :closed = Loopback.recv_frame(c)
    end
  end

  describe "length-prefixed framing" do
    test "wraps and reads back one frame, leaving the tail" do
      {:ok, framed} = Carrier.frame_length_prefixed("hi")
      assert framed == <<0, 0, 0, 2, ?h, ?i>>
      assert {:ok, "hi", "rest"} = Carrier.read_length_prefixed(framed <> "rest")
    end

    test "a short buffer is incomplete, not an error" do
      assert :incomplete = Carrier.read_length_prefixed(<<0, 0, 0, 4, 1, 2>>)
    end

    test "the 16 MiB guard rejects an oversized frame before allocating" do
      max = Conventions.max_frame_default()

      assert {:error, {:frame_too_large, _, ^max}} =
               Carrier.read_length_prefixed(<<0xFF, 0xFF, 0xFF, 0xFF>>)

      big = :binary.copy(<<0>>, 8)
      assert {:error, {:frame_too_large, 8, 4}} = Carrier.frame_length_prefixed(big, 4)
    end
  end

  describe "sequence tracker" do
    test "first observation is :first, not :restart" do
      {outcome, t} = Datagrams.observe(Datagrams.new_tracker(), 5, nil)
      assert outcome == :first
      assert t.last_seq == 5
    end

    test "no-epoch to first-epoch is not a restart" do
      {_, t} = Datagrams.observe(Datagrams.new_tracker(), 1, nil)
      {outcome, _} = Datagrams.observe(t, 2, 3)
      assert outcome == {:advanced, 0}
    end

    test "a changed prior epoch is a restart" do
      {_, t} = Datagrams.observe(Datagrams.new_tracker(), 9, 1)
      {outcome, t} = Datagrams.observe(t, 1, 2)
      assert outcome == :restart
      assert t.last_seq == 1
    end

    test "gaps, duplicates, and unsequenced datagrams classify correctly" do
      {_, t} = Datagrams.observe(Datagrams.new_tracker(), 1, nil)
      {gap, t} = Datagrams.observe(t, 4, nil)
      assert gap == {:advanced, 2}
      {dup, t} = Datagrams.observe(t, 2, nil)
      assert dup == :late_or_duplicate
      {unseq, _} = Datagrams.observe(t, 0, nil)
      assert unseq == {:advanced, 0}
    end
  end
end
