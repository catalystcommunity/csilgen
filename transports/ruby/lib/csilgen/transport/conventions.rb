# frozen_string_literal: true

module Csilgen
  module Transport
    # Conventions shared by every CSIL transport (see csil-transport-conventions.md):
    # the version constant, the status registry, tag-24 payload helpers, the max-frame
    # guard, and the typed-accessor helpers the three transports build envelopes from.
    module Conventions
      # Current transport version. A new value is minted only for a breaking change to
      # envelope layout or semantics.
      VERSION = 1

      # CBOR semantic tag wrapping an embedded, opaque CBOR item (RFC 8949 3.4.5.1).
      TAG_ENCODED_CBOR = 24

      # Reserved service ordinal for the transport control plane (Events lifecycle).
      CONTROL_SERVICE_ORD = 0

      # Default max encoded envelope size for stream/message carriers (16 MiB). A
      # carrier rejects anything larger before allocating for it.
      MAX_FRAME_DEFAULT = 16 * 1024 * 1024

      STATUS_NAMES = {
        0 => "ok",
        1 => "malformed-envelope",
        2 => "unknown-service-or-op",
        3 => "unauthenticated",
        4 => "forbidden",
        5 => "version-unsupported",
        6 => "internal",
        7 => "unavailable",
        8 => "deadline-exceeded"
      }.freeze

      module_function

      # Wrap opaque payload bytes (themselves a CBOR item) in tag 24. The bytes are
      # forced binary so the codec treats them as a byte string, never re-encoded text.
      def tag24(payload)
        CBOR::Tag.new(tag: TAG_ENCODED_CBOR, value: payload.b)
      end

      # Extract the opaque payload bytes from a tag-24 value.
      def untag24(value)
        unless value.is_a?(CBOR::Tag) && value.tag == TAG_ENCODED_CBOR
          raise MalformedError, "expected a tag-24 (encoded-cbor) payload"
        end
        raise MalformedError, "tag-24 payload is not a byte string" unless value.value.is_a?(::String)

        value.value.b
      end

      def encode_value(value)
        CBOR.encode(value)
      rescue CBOR::CBORError => e
        raise EncodeError, e.message
      end

      def decode_value(data)
        CBOR.decode(data)
      rescue CBOR::CBORError => e
        raise DecodeError, e.message
      end

      # Build a map from key/value pairs. The encoder sorts entries into RFC 8949
      # deterministic order at encode time, so callers may append in any order.
      def canon_map(entries)
        entries.to_h
      end

      def map_get(map_value, key)
        map_value.is_a?(::Hash) ? map_value[key] : nil
      end

      def get_uint(map_value, key)
        value = map_get(map_value, key)
        return value if value.is_a?(::Integer) && value >= 0

        raise MalformedError, "missing or non-integer field '#{key}'"
      end

      def get_int(map_value, key)
        value = map_get(map_value, key)
        return value if value.is_a?(::Integer)

        raise MalformedError, "missing or non-integer field '#{key}'"
      end

      def get_text(map_value, key)
        value = map_get(map_value, key)
        return value if value.is_a?(::String)

        raise MalformedError, "missing or non-text field '#{key}'"
      end

      def get_text_opt(map_value, key)
        value = map_get(map_value, key)
        value.is_a?(::String) ? value : nil
      end

      def get_uint_opt(map_value, key)
        value = map_get(map_value, key)
        (value.is_a?(::Integer) && value >= 0) ? value : nil
      end

      def check_version(version)
        raise UnsupportedVersionError.new(version) if version != VERSION
      end

      def hex_to_bytes(text)
        [text].pack("H*")
      end

      def bytes_to_hex(data)
        data.unpack1("H*")
      end
    end

    # Transport-level status. Distinct from application errors, which ride inside the
    # payload as a declared `/ ErrorType` arm. Codes 0..8 are the registry; any other
    # code (host extensions at >= 64, or an unrecognized value) is carried as-is.
    Status = ::Data.define(:code) do
      def self.from_code(code)
        new(code: code.to_i)
      end

      def name
        Conventions::STATUS_NAMES.fetch(code, "other")
      end

      def ok?
        code.zero?
      end
    end

    # Registry constants (mirror the Rust `Status` variants).
    OK = Status.new(code: 0)
    MALFORMED_ENVELOPE = Status.new(code: 1)
    UNKNOWN_SERVICE_OR_OP = Status.new(code: 2)
    UNAUTHENTICATED = Status.new(code: 3)
    FORBIDDEN = Status.new(code: 4)
    VERSION_UNSUPPORTED = Status.new(code: 5)
    INTERNAL = Status.new(code: 6)
    UNAVAILABLE = Status.new(code: 7)
    DEADLINE_EXCEEDED = Status.new(code: 8)

    # Error classes surfaced by the transport layer (the wire), distinct from a host's
    # application errors (which ride in the payload). All descend from Error so a host
    # can rescue the whole family at once.
    class EncodeError < Error; end
    class DecodeError < Error; end
    class MalformedError < Error; end
    class CarrierError < Error; end

    class FrameTooLargeError < Error
      attr_reader :got, :maximum

      def initialize(got, maximum)
        @got = got
        @maximum = maximum
        super("frame of #{got} bytes exceeds max-frame guard of #{maximum} bytes")
      end
    end

    class UnsupportedVersionError < Error
      attr_reader :version

      def initialize(version)
        @version = version
        super("unsupported transport version #{version}")
      end
    end

    # A non-zero transport status returned by the peer.
    class StatusError < Error
      attr_reader :status, :code, :message

      def initialize(status, message = nil)
        @status = status
        @code = status.code
        @message = message
        text = "transport status #{status.name} (#{status.code})"
        text += ": #{message}" if message
        super(text)
      end
    end
  end
end
