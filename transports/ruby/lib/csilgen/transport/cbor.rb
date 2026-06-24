# frozen_string_literal: true

module Csilgen
  module Transport
    # Minimal canonical CBOR codec (RFC 8949), hand-rolled with zero gem deps so the
    # transport stays offline-testable. It supports exactly what CSIL envelopes need --
    # unsigned ints, negative ints, byte strings, text strings, arrays, maps, and tag
    # 24 -- and nothing else. Maps use core deterministic encoding (entries sorted by
    # the bytewise order of their *encoded* keys) so bytes match the conformance
    # vectors byte for byte across every language's reference library.
    module CBOR
      # Raised for malformed input or any construct outside the supported subset. A
      # subclass of the transport Error so a host can rescue the whole family at once.
      class CBORError < Error; end

      # A CBOR semantic tag wrapping one data item (RFC 8949 3.4). Data gives us the
      # structural == the conformance decode checks rely on for free.
      Tag = ::Data.define(:tag, :value)

      module_function

      # The signed-64 floor expressed as a negative-int argument ceiling. A major-1
      # argument above this names a value below i64::MIN; Go's reference rejects it
      # rather than letting the magnitude wrap, and we mirror that discipline even
      # though Ruby integers would not overflow, so behavior is identical cross-language.
      MAX_NINT_ARG = (1 << 63) - 1

      # Largest argument any CBOR head can carry; a larger Ruby Integer would make
      # pack("Q>") raise RangeError, which we convert into a clean CBORError instead.
      MAX_ARG = (1 << 64) - 1

      def encode(value)
        out = +"".b
        encode_into(out, value)
        out
      end

      def encode_into(out, value)
        case value
        when Tag
          encode_head(out, 6, value.tag)
          encode_into(out, value.value)
        when ::Integer
          if value.negative?
            # CBOR negative ints encode -1-n; the head argument is the magnitude minus one.
            encode_head(out, 1, -1 - value)
          else
            encode_head(out, 0, value)
          end
        when ::String
          # Binary-encoded strings are opaque bytes (major 2); anything else is text
          # (major 3). Distinguishing by encoding is the natural Ruby idiom and keeps
          # payload bytes from being re-validated as UTF-8.
          if value.encoding == ::Encoding::BINARY
            encode_head(out, 2, value.bytesize)
            out << value
          else
            encoded = value.encode("UTF-8")
            encode_head(out, 3, encoded.bytesize)
            out << encoded.b
          end
        when ::Array
          encode_head(out, 4, value.length)
          value.each { |item| encode_into(out, item) }
        when ::Hash
          # Deterministic map: sort entries by their encoded-key bytes. Because the key
          # head byte carries the length, shorter keys sort first -- the same
          # length-then-bytewise order RFC 8949 4.2.1 mandates. Keys are forced binary
          # so String#<=> compares bytewise rather than by the source encoding.
          items = value.map { |k, v| [encode(k), v] }
          items.sort_by! { |k, _| k }
          encode_head(out, 5, items.length)
          items.each do |key_bytes, v|
            out << key_bytes
            encode_into(out, v)
          end
        when true, false
          raise CBORError, "booleans are not part of any CSIL transport envelope"
        else
          raise CBORError, "cannot encode value of type #{value.class}"
        end
      end

      def encode_head(out, major, arg)
        raise CBORError, "CBOR argument #{arg} exceeds 64-bit range" if arg > MAX_ARG

        mt = major << 5
        if arg < 24
          out << [mt | arg].pack("C")
        elsif arg < 0x100
          out << [mt | 24, arg].pack("CC")
        elsif arg < 0x1_0000
          out << [mt | 25].pack("C") << [arg].pack("n")
        elsif arg < 0x1_0000_0000
          out << [mt | 26].pack("C") << [arg].pack("N")
        else
          out << [mt | 27].pack("C") << [arg].pack("Q>")
        end
      end

      # Decode exactly one CBOR item and reject trailing bytes: a CSIL envelope is a
      # single self-contained item, so leftover bytes are a malformed frame.
      def decode(data)
        data = data.b
        value, pos = decode_at(data, 0)
        raise CBORError, "trailing bytes after CBOR item" if pos != data.bytesize

        value
      end

      def decode_at(data, pos)
        need(data, pos, 1)
        initial = data.getbyte(pos)
        pos += 1
        major = initial >> 5
        info = initial & 0x1F
        arg, pos = read_argument(data, pos, info)

        case major
        when 0
          [arg, pos]
        when 1
          raise CBORError, "negative integer out of signed-64 range" if arg > MAX_NINT_ARG

          [-1 - arg, pos]
        when 2
          need(data, pos, arg)
          [data.byteslice(pos, arg), pos + arg]
        when 3
          need(data, pos, arg)
          text = data.byteslice(pos, arg).force_encoding("UTF-8")
          raise CBORError, "invalid UTF-8 in text string" unless text.valid_encoding?

          [text, pos + arg]
        when 4
          items = []
          arg.times do
            item, pos = decode_at(data, pos)
            items << item
          end
          [items, pos]
        when 5
          result = {}
          arg.times do
            key, pos = decode_at(data, pos)
            val, pos = decode_at(data, pos)
            result[key] = val
          end
          [result, pos]
        when 6
          inner, pos = decode_at(data, pos)
          [Tag.new(tag: arg, value: inner), pos]
        else
          raise CBORError, "unsupported CBOR major type #{major}"
        end
      end

      def read_argument(data, pos, info)
        if info < 24
          [info, pos]
        elsif info == 24
          need(data, pos, 1)
          [data.getbyte(pos), pos + 1]
        elsif info == 25
          need(data, pos, 2)
          [data.byteslice(pos, 2).unpack1("n"), pos + 2]
        elsif info == 26
          need(data, pos, 4)
          [data.byteslice(pos, 4).unpack1("N"), pos + 4]
        elsif info == 27
          need(data, pos, 8)
          [data.byteslice(pos, 8).unpack1("Q>"), pos + 8]
        else
          # 28..30 are reserved and 31 is indefinite-length, both forbidden in a CSIL envelope.
          raise CBORError, "unsupported additional-info value #{info}"
        end
      end

      def need(data, pos, n)
        raise CBORError, "unexpected end of CBOR input" if pos + n > data.bytesize
      end
    end
  end
end
