# frozen_string_literal: true

require "socket"

module Csilgen
  module Transport
    # Carrier seams -- the bring-your-own-carrier boundary (conventions doc section 7).
    #
    # The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
    # byte/datagram transport) is injected. A host supplies QUIC, WebRTC, a platform
    # media stack, or anything else without changing the library. There is no interface
    # keyword in Ruby: a carrier is any duck-typed object responding to the right pair
    # of methods.
    #
    # A *frame carrier* (RPC / Events) responds to `send_frame(bytes)` and
    # `recv_frame -> bytes | nil` (nil at a clean end of stream). A *datagram carrier*
    # (Datagrams) responds to `send_datagram(bytes)` and `recv_datagram -> bytes | nil`.
    module Carrier
      include Conventions

      module_function

      def read_exact(reader, n)
        buf = +"".b
        while buf.bytesize < n
          chunk = reader.read(n - buf.bytesize)
          return nil if chunk.nil? || chunk.empty?

          buf << chunk.b
        end
        buf
      end

      # Write a 4-byte big-endian length prefix followed by the frame (CSIL stream
      # framing). Rejects an over-limit frame before writing anything.
      def write_length_prefixed(writer, frame, maximum = Conventions::MAX_FRAME_DEFAULT)
        frame = frame.b
        raise FrameTooLargeError.new(frame.bytesize, maximum) if frame.bytesize > maximum

        writer.write([frame.bytesize].pack("N"))
        writer.write(frame)
        writer.flush
      end

      # Read one length-prefixed frame, enforcing the max-frame guard *before*
      # allocating so a hostile length prefix cannot force a large allocation. Returns
      # nil at a clean EOF before any byte of a frame.
      def read_length_prefixed(reader, maximum = Conventions::MAX_FRAME_DEFAULT)
        prefix = read_exact(reader, 4)
        return nil if prefix.nil?

        length = prefix.unpack1("N")
        raise FrameTooLargeError.new(length, maximum) if length > maximum

        body = read_exact(reader, length)
        raise CarrierError, "connection closed mid-frame" if body.nil?

        body
      end
    end

    # A FrameCarrier over any read/write/flush byte stream (a socket, a TLS-wrapped IO,
    # a StringIO pair), using the canonical 4-byte length-prefix framing.
    class StreamCarrier
      attr_reader :stream, :max_frame

      def initialize(stream, max_frame: Conventions::MAX_FRAME_DEFAULT)
        @stream = stream
        # Validated here rather than at the first frame, so a misconfigured carrier
        # is a construction-time error the host can surface at startup.
        @max_frame = Conventions.validate_max_frame(max_frame)
      end

      def send_frame(frame)
        Carrier.write_length_prefixed(@stream, frame, @max_frame)
      end

      def recv_frame
        Carrier.read_length_prefixed(@stream, @max_frame)
      end
    end

    # An in-memory FrameCarrier backed by frame queues -- for tests and for driving the
    # codec without a socket. A Mutex keeps it safe when a host hands the two ends to
    # separate threads; recv is non-blocking and returns nil when empty (a clean EOF).
    class LoopbackFrameCarrier
      def initialize
        @outbound = []
        @inbound = []
        @lock = Mutex.new
      end

      def push_inbound(frame)
        @lock.synchronize { @inbound << frame.b }
      end

      def take_outbound
        @lock.synchronize { @outbound.shift }
      end

      def send_frame(frame)
        @lock.synchronize { @outbound << frame.b }
      end

      def recv_frame
        @lock.synchronize { @inbound.shift }
      end
    end

    # An in-memory DatagramCarrier -- for tests and codec drives.
    class LoopbackDatagramCarrier
      def initialize
        @outbound = []
        @inbound = []
        @lock = Mutex.new
      end

      def push_inbound(datagram)
        @lock.synchronize { @inbound << datagram.b }
      end

      def take_outbound
        @lock.synchronize { @outbound.shift }
      end

      def send_datagram(datagram)
        @lock.synchronize { @outbound << datagram.b }
      end

      def recv_datagram
        @lock.synchronize { @inbound.shift }
      end
    end

    # A UDP-backed DatagramCarrier. Built-in convenience for the native datagram path;
    # the browser path (WebRTC unreliable / WebTransport) implements the same duck-typed
    # contract in the host.
    class UdpDatagramCarrier
      def initialize(socket, recv_size: 1200)
        @socket = socket
        @recv_size = recv_size
      end

      def send_datagram(datagram)
        @socket.send(datagram.b, 0)
      rescue SystemCallError => e
        raise CarrierError, e.message
      end

      def recv_datagram
        data, = @socket.recvfrom(@recv_size)
        data.b
      rescue SystemCallError => e
        raise CarrierError, e.message
      end
    end
  end
end
