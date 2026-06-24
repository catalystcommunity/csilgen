# frozen_string_literal: true

module Csilgen
  module Transport
    # CSIL-Events transport -- typed bidirectional event streams -- see
    # csil-events-transport.md. Verbose (text-keyed) and compact (positional) profiles,
    # plus the control plane (service ordinal 0) lifecycle payloads.
    module Events
      # Which wire profile a connection uses for its lifetime, fixed by `hello`.
      # Symbols are the lightest idiomatic enum in Ruby.
      module Profile
        VERBOSE = :verbose
        COMPACT = :compact
        ALL = [VERBOSE, COMPACT].freeze

        module_function

        def parse(text)
          case text
          when "verbose" then VERBOSE
          when "compact" then COMPACT
          end
        end

        def as_str(profile)
          profile.to_s
        end
      end

      # One typed event flowing in either direction. Identified by service+operation
      # (verbose) or by their ordinals (compact); carries an optional correlation `id`
      # when it is a request expecting a reply, or that reply. The compact ordinals are
      # full CBOR uints (events doc section 2), unlike the fixed-width datagram header.
      Event = ::Data.define(:service, :service_ord, :event, :op_ord, :id, :payload) do
        def initialize(service: nil, service_ord: nil, event: nil, op_ord: nil, id: nil, payload: "".b)
          super
        end

        def self.verbose(service, event, payload)
          new(service: service, event: event, payload: payload)
        end

        def self.compact(service_ord, op_ord, payload)
          new(service_ord: service_ord, op_ord: op_ord, payload: payload)
        end

        def with_id(new_id) = with(id: new_id)

        def encode(profile)
          (profile == Profile::VERBOSE) ? encode_verbose : encode_compact
        end

        def encode_verbose
          raise MalformedError, "verbose event missing 'event' name" if event.nil?

          entries = [
            ["event", event],
            ["payload", Conventions.tag24(payload)]
          ]
          entries << ["service", service] unless service.nil?
          entries << ["id", id] unless id.nil?
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def encode_compact
          raise MalformedError, "compact event missing service ordinal" if service_ord.nil?
          raise MalformedError, "compact event missing op ordinal" if op_ord.nil?

          arr = [service_ord, op_ord]
          arr << id unless id.nil?
          arr << Conventions.tag24(payload)
          Conventions.encode_value(arr)
        end

        def self.decode(data, profile)
          (profile == Profile::VERBOSE) ? decode_verbose(data) : decode_compact(data)
        end

        def self.decode_verbose(data)
          value = Conventions.decode_value(data)
          payload_value = Conventions.map_get(value, "payload")
          raise MalformedError, "missing 'payload'" if payload_value.nil?

          new(
            service: Conventions.get_text_opt(value, "service"),
            event: Conventions.get_text(value, "event"),
            id: Conventions.get_uint_opt(value, "id"),
            payload: Conventions.untag24(payload_value)
          )
        end

        def self.decode_compact(data)
          value = Conventions.decode_value(data)
          raise MalformedError, "compact event is not an array" unless value.is_a?(::Array)

          # 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
          case value.length
          when 3
            service_ord, op_ord, payload_value = value
            id_value = nil
          when 4
            service_ord, op_ord, id_value, payload_value = value
          else
            raise MalformedError, "compact event array has #{value.length} elements, expected 3 or 4"
          end

          as_uint = lambda do |val|
            raise MalformedError, "ordinal is not a non-negative integer" unless val.is_a?(::Integer) && val >= 0

            val
          end

          new(
            service_ord: as_uint.call(service_ord),
            op_ord: as_uint.call(op_ord),
            id: id_value.nil? ? nil : as_uint.call(id_value),
            payload: Conventions.untag24(payload_value)
          )
        end
      end

      # Control-plane operation ordinals (under service ordinal 0) and their `$`-sigil
      # verbose names.
      module Control
        HELLO = 0
        HELLO_ACK = 1
        PING = 2
        PONG = 3
        CLOSE = 4
        ERROR = 5

        HELLO_NAME = "$hello"
        HELLO_ACK_NAME = "$hello-ack"
        PING_NAME = "$ping"
        PONG_NAME = "$pong"
        CLOSE_NAME = "$close"
        ERROR_NAME = "$error"
      end

      # The `$hello` payload offered by the connection initiator.
      Hello = ::Data.define(:versions, :profiles, :service, :auth) do
        def initialize(versions:, profiles:, service: nil, auth: nil)
          super
        end

        def encode
          entries = [
            ["versions", versions.dup],
            ["profiles", profiles.dup]
          ]
          entries << ["service", service] unless service.nil?
          entries << ["auth", auth] unless auth.nil?
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          versions_value = Conventions.map_get(value, "versions")
          raise MalformedError, "hello missing 'versions'" unless versions_value.is_a?(::Array)

          versions = versions_value.select { |v| v.is_a?(::Integer) && v >= 0 }
          profiles_value = Conventions.map_get(value, "profiles")
          raise MalformedError, "hello missing 'profiles'" unless profiles_value.is_a?(::Array)

          profiles = profiles_value.select { |p| p.is_a?(::String) }
          new(
            versions: versions,
            profiles: profiles,
            service: Conventions.get_text_opt(value, "service"),
            auth: Conventions.get_text_opt(value, "auth")
          )
        end

        # Select a profile from this hello's offers, honoring the peer's preference
        # order and what the server supports. Returns the chosen [version, profile], or
        # nil if nothing is mutually supported.
        def negotiate(supported)
          return nil unless versions.include?(Conventions::VERSION)

          profiles.each do |offered|
            parsed = Profile.parse(offered)
            return [Conventions::VERSION, parsed] if parsed && supported.include?(parsed)
          end
          nil
        end
      end

      # The `$hello-ack` payload returned by the peer.
      HelloAck = ::Data.define(:v, :profile, :session) do
        def initialize(v:, profile:, session: nil)
          super
        end

        def encode
          entries = [
            ["v", v],
            ["profile", profile]
          ]
          entries << ["session", session] unless session.nil?
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          new(
            v: Conventions.get_uint(value, "v"),
            profile: Conventions.get_text(value, "profile"),
            session: Conventions.get_text_opt(value, "session")
          )
        end
      end

      # A `$ping` / `$pong` heartbeat payload.
      Heartbeat = ::Data.define(:nonce, :at) do
        def initialize(nonce:, at: nil)
          super
        end

        def encode
          entries = [["nonce", nonce]]
          entries << ["at", at] unless at.nil?
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          new(nonce: Conventions.get_uint(value, "nonce"), at: Conventions.get_uint_opt(value, "at"))
        end
      end

      # A `$close` payload.
      Close = ::Data.define(:status, :reason) do
        def initialize(status:, reason: nil)
          super
        end

        def encode
          entries = [["status", status.code]]
          entries << ["reason", reason] unless reason.nil?
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          new(
            status: Status.from_code(Conventions.get_int(value, "status")),
            reason: Conventions.get_text_opt(value, "reason")
          )
        end
      end
    end
  end
end
