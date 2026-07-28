# frozen_string_literal: true

require_relative "test_helper"
require "stringio"

# The configurable max-frame guard (conventions doc section 5): a host sets the limit
# up or down through the carrier's public API, the limit applies to reads and writes
# alike, an oversized inbound length is rejected before allocation, and an invalid
# limit fails at construction rather than on the first frame.
module Csilgen
  module Transport
    # An in-memory duplex: writes land in a buffer that subsequent reads drain.
    # +read_count+ lets a test prove the guard fires before a frame body is pulled.
    class CountingDuplex
      attr_reader :read_count

      def initialize(initial = "")
        @buf = +initial.b
        @pos = 0
        @read_count = 0
      end

      def read(n)
        chunk = @buf.byteslice(@pos, n) || ""
        @pos += chunk.bytesize
        @read_count += chunk.bytesize
        chunk.empty? ? nil : chunk
      end

      def write(data)
        @buf << data.b
        data.bytesize
      end

      def flush; end

      def written
        @buf
      end
    end

    class MaxFrameTest < Minitest::Test
      def test_default_limit_accepts_frame_below_it
        carrier = StreamCarrier.new(CountingDuplex.new)
        frame = "\xAB".b * 1024
        carrier.send_frame(frame)
        assert_equal frame, carrier.recv_frame
      end

      def test_default_limit_rejects_frame_above_it
        stream = CountingDuplex.new
        carrier = StreamCarrier.new(stream)
        err = assert_raises(FrameTooLargeError) do
          carrier.send_frame("\x00".b * (Conventions::MAX_FRAME_DEFAULT + 1))
        end
        assert_equal Conventions::MAX_FRAME_DEFAULT, err.maximum
        assert_equal "", stream.written, "a rejected frame must not put bytes on the wire"
      end

      def test_larger_custom_limit_accepts_what_default_rejects
        carrier = StreamCarrier.new(
          CountingDuplex.new, max_frame: Conventions::MAX_FRAME_DEFAULT + 4096
        )
        frame = "\x00".b * (Conventions::MAX_FRAME_DEFAULT + 1)
        carrier.send_frame(frame)
        assert_equal frame, carrier.recv_frame
      end

      def test_smaller_custom_limit_rejects_what_default_accepts
        carrier = StreamCarrier.new(CountingDuplex.new, max_frame: 64)
        assert_raises(FrameTooLargeError) { carrier.send_frame("\xCD".b * 1024) }
      end

      def test_oversized_incoming_length_rejected_before_allocation
        # A prefix claiming ~4 GiB followed by no body: if the guard ran after the
        # read this would allocate; it must fail on the prefix alone.
        stream = CountingDuplex.new("\xFF\xFF\xFF\xFF".b)
        carrier = StreamCarrier.new(stream, max_frame: 4096)
        assert_raises(FrameTooLargeError) { carrier.recv_frame }
        assert_equal 4, stream.read_count, "guard must fire on the 4-byte prefix alone"
      end

      def test_invalid_limits_rejected_at_construction
        [0, -1, -4096, Conventions::MAX_FRAME_LIMIT + 1, 1 << 40, "4096", 1.5, nil].each do |limit|
          assert_raises(InvalidMaxFrameError, "limit #{limit.inspect} must be rejected") do
            StreamCarrier.new(CountingDuplex.new, max_frame: limit)
          end
        end
      end

      def test_boundary_limits_accepted
        [1, Conventions::MAX_FRAME_DEFAULT, Conventions::MAX_FRAME_LIMIT].each do |limit|
          carrier = StreamCarrier.new(CountingDuplex.new, max_frame: limit)
          assert_equal limit, carrier.max_frame
        end
      end

      def test_guard_works_over_a_real_stringio
        # The guard is not tied to the test double: a StringIO behaves the same.
        io = StringIO.new(+"".b)
        StreamCarrier.new(io, max_frame: 1024).send_frame("hello".b)
        io.rewind
        assert_equal "hello".b, StreamCarrier.new(io, max_frame: 1024).recv_frame
      end
    end
  end
end
