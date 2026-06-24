# frozen_string_literal: true

module Csilgen
  module Transport
    # CSIL-Datagrams transport -- unreliable, unordered, message-oriented -- see
    # csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header
    # profiles. A datagram channel is single-service: the service is bound at channel
    # setup, so datagrams carry no service ordinal.
    module Datagrams
      # Conservative max datagram size (envelope + payload) safe across UDP/WebRTC/QUIC.
      MAX_DATAGRAM_DEFAULT = 1200

      COMPACT_VER = 1
      FLAG_EPOCH = 0b0001

      # A datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload]. Here
      # the integers are full CBOR uints, in contrast to the fixed-width compact header.
      Datagram = ::Data.define(:op_ord, :seq, :payload) do
        def initialize(op_ord:, seq:, payload: "".b)
          super
        end

        def encode
          Conventions.encode_value([Conventions::VERSION, op_ord, seq, Conventions.tag24(payload)])
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          raise MalformedError, "datagram is not an array" unless value.is_a?(::Array)
          raise MalformedError, "datagram array has #{value.length} elements, expected 4" if value.length != 4

          as_uint = lambda do |val|
            unless val.is_a?(::Integer) && val >= 0
              raise MalformedError, "datagram field is not a non-negative integer"
            end

            val
          end

          Conventions.check_version(as_uint.call(value[0]))
          new(
            op_ord: as_uint.call(value[1]),
            seq: as_uint.call(value[2]),
            payload: Conventions.untag24(value[3])
          )
        end
      end

      # A datagram in the compact fixed-header profile. Header layout (datagrams doc):
      # [ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8]) then the opaque body. The widths
      # are fixed and not visible in the conformance JSON, so they are pinned here.
      CompactDatagram = ::Data.define(:op_ord, :seq, :epoch, :body) do
        def initialize(op_ord:, seq:, epoch: nil, body: "".b)
          super
        end

        def with_epoch(new_epoch) = with(epoch: new_epoch)

        def encode
          flags = epoch.nil? ? 0 : FLAG_EPOCH
          out = +"".b
          out << [(COMPACT_VER << 4) | (flags & 0x0F)].pack("C")
          out << [op_ord & 0xFF].pack("C")
          out << [seq & 0xFFFF].pack("n")
          out << [epoch & 0xFF].pack("C") unless epoch.nil?
          out << body.b
          out
        end

        def self.decode(data)
          data = data.b
          raise MalformedError, "compact datagram shorter than the 4-byte header" if data.bytesize < 4

          ver = data.getbyte(0) >> 4
          raise UnsupportedVersionError.new(ver) if ver != COMPACT_VER

          flags = data.getbyte(0) & 0x0F
          op_ord = data.getbyte(1)
          seq = data.byteslice(2, 2).unpack1("n")
          if flags & FLAG_EPOCH != 0
            raise MalformedError, "compact datagram flags claim an epoch byte that is absent" if data.bytesize < 5

            epoch = data.getbyte(4)
            body_start = 5
          else
            epoch = nil
            body_start = 4
          end
          new(op_ord: op_ord, seq: seq, epoch: epoch, body: data.byteslice(body_start..) || "".b)
        end
      end

      # Classification of an incoming sequence number relative to what was last seen,
      # for loss/reorder/restart detection. The transport detects; the app decides.
      # `gap` is only meaningful for `advanced`: how many sequence numbers were skipped.
      SeqEvent = ::Data.define(:kind, :gap) do
        def initialize(kind:, gap: 0)
          super
        end
      end

      module_function

      def first = SeqEvent.new(kind: "first")

      def advanced(gap) = SeqEvent.new(kind: "advanced", gap: gap)

      def late_or_duplicate = SeqEvent.new(kind: "late_or_duplicate")

      def restart = SeqEvent.new(kind: "restart")

      # Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced
      # datagrams (seq 0) are reported as advanced(gap=0) once a first is seen.
      class SeqTracker
        def initialize
          @last_seq = nil
          @last_epoch = nil
        end

        def observe(seq, epoch = nil)
          # A restart fires only when a *prior* epoch existed and changed; going from
          # no-epoch to the first epoch is not a restart.
          if epoch != @last_epoch && !@last_epoch.nil?
            @last_epoch = epoch
            @last_seq = seq
            return Datagrams.restart
          end
          @last_epoch = epoch
          # seq 0 marks an unsequenced datagram: no ordering information, so it is never
          # late or duplicate. Report a zero-gap advance and leave the running sequence
          # untouched.
          return Datagrams.advanced(0) if seq.zero?

          if @last_seq.nil?
            @last_seq = seq
            return Datagrams.first
          end
          if seq > @last_seq
            gap = seq - @last_seq - 1
            @last_seq = seq
            return Datagrams.advanced(gap)
          end
          Datagrams.late_or_duplicate
        end
      end
    end
  end
end
