# frozen_string_literal: true

module Csilgen
  module Transport
    # CSIL-RPC transport -- request/response/push envelopes -- see csil-rpc-transport.md.
    module RPC
      # A CSIL-RPC request (client -> server). `payload` is opaque CBOR(request type)
      # bytes, wrapped in tag 24 on the wire.
      RpcRequest = ::Data.define(:service, :op, :id, :payload, :auth) do
        # Data's generated initializer has no default-argument support; this override
        # supplies the optional fields' defaults (an absent id/auth is omitted on the wire).
        def initialize(service:, op:, payload: "".b, id: nil, auth: nil)
          super
        end

        def with_id(new_id) = with(id: new_id)

        def with_auth(new_auth) = with(auth: new_auth)

        def encode
          entries = [
            ["v", Conventions::VERSION],
            ["service", service],
            ["op", op],
            ["payload", Conventions.tag24(payload)]
          ]
          entries << ["id", id] unless id.nil?
          entries << ["auth", auth] unless auth.nil?
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          Conventions.check_version(Conventions.get_uint(value, "v"))
          payload_value = Conventions.map_get(value, "payload")
          raise MalformedError, "missing 'payload'" if payload_value.nil?

          new(
            service: Conventions.get_text(value, "service"),
            op: Conventions.get_text(value, "op"),
            id: Conventions.get_uint_opt(value, "id"),
            payload: Conventions.untag24(payload_value),
            auth: Conventions.get_text_opt(value, "auth")
          )
        end
      end

      # A CSIL-RPC response (server -> client). `variant` names the declared
      # output-choice arm `payload` decodes to (the CSIL type name); `payload` is empty
      # when `status` is non-zero -- but it is still a *present* tag-24 byte string.
      RpcResponse = ::Data.define(:id, :status, :variant, :error, :payload) do
        def initialize(status:, id: nil, variant: nil, error: nil, payload: "".b)
          super
        end

        def self.ok(variant, payload)
          new(id: nil, status: Status.new(code: 0), variant: variant, error: nil, payload: payload)
        end

        def self.transport_error(status, message)
          new(id: nil, status: status, variant: nil, error: message, payload: "".b)
        end

        def with_id(new_id) = with(id: new_id)

        def encode
          entries = [
            ["v", Conventions::VERSION],
            ["status", status.code],
            ["payload", Conventions.tag24(payload)]
          ]
          entries << ["id", id] unless id.nil?
          entries << ["variant", variant] unless variant.nil?
          entries << ["error", error] unless error.nil?
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          Conventions.check_version(Conventions.get_uint(value, "v"))
          payload_value = Conventions.map_get(value, "payload")
          payload = payload_value.nil? ? "".b : Conventions.untag24(payload_value)
          new(
            id: Conventions.get_uint_opt(value, "id"),
            status: Status.from_code(Conventions.get_int(value, "status")),
            variant: Conventions.get_text_opt(value, "variant"),
            error: Conventions.get_text_opt(value, "error"),
            payload: payload
          )
        end

        # Return self when the status is ok; otherwise raise StatusError. Callers use
        # this after decode to surface transport failures distinctly from application
        # errors (which ride as a status-0 typed variant).
        def into_transport_error
          return self if status.ok?

          raise StatusError.new(status, error)
        end
      end

      # A CSIL-RPC server push (server -> client) for `<-` operations.
      RpcPush = ::Data.define(:service, :event, :payload) do
        def initialize(service:, event:, payload: "".b)
          super
        end

        def encode
          entries = [
            ["v", Conventions::VERSION],
            ["service", service],
            ["event", event],
            ["payload", Conventions.tag24(payload)]
          ]
          Conventions.encode_value(Conventions.canon_map(entries))
        end

        def self.decode(data)
          value = Conventions.decode_value(data)
          Conventions.check_version(Conventions.get_uint(value, "v"))
          payload_value = Conventions.map_get(value, "payload")
          raise MalformedError, "missing 'payload'" if payload_value.nil?

          new(
            service: Conventions.get_text(value, "service"),
            event: Conventions.get_text(value, "event"),
            payload: Conventions.untag24(payload_value)
          )
        end
      end

      # A successful handler outcome: the chosen variant name and encoded payload.
      Reply = ::Data.define(:variant, :payload)

      # A transport-level handler outcome (no typed payload).
      TransportOutcome = ::Data.define(:status, :message)

      # A CSIL-RPC client over a frame carrier. The carrier is injected (bring your
      # own); the client owns the envelope and a per-connection monotonic correlation id.
      class Client
        attr_reader :carrier

        def initialize(carrier, multiplexed:)
          # `multiplexed` true assigns a correlation id to every request (required on
          # WS / pipelined streams); false omits it (one-in-flight carriers).
          @carrier = carrier
          @next_id = 1
          @multiplexed = multiplexed
        end

        # Invoke `service`/`op` with an encoded request payload, returning the decoded
        # response. A non-zero transport status is raised as StatusError.
        def call(service, op, payload, auth: nil)
          req = RpcRequest.new(service: service, op: op, payload: payload, auth: auth)
          if @multiplexed
            req = req.with_id(@next_id)
            @next_id += 1
          end
          @carrier.send_frame(req.encode)
          frame = @carrier.recv_frame
          raise CarrierError, "connection closed before response" if frame.nil?

          RpcResponse.decode(frame).into_transport_error
        end
      end

      # A CSIL-RPC server over a frame carrier. The host supplies a handler mapping an
      # RpcRequest to an outcome (any object responding to `call`); the generated router
      # is the natural implementation of that handler.
      class Server
        def initialize(carrier)
          @carrier = carrier
        end

        # Read one request, dispatch it through `handler`, and write the response.
        # Returns false at a clean end of stream.
        def serve_one(handler)
          frame = @carrier.recv_frame
          return false if frame.nil?

          begin
            req = RpcRequest.decode(frame)
          rescue Error => e
            # A malformed envelope cannot be correlated, so it returns transport status 1.
            resp = RpcResponse.transport_error(Status.new(code: 1), e.message)
          else
            outcome = handler.call(req)
            resp = case outcome
            in Reply(variant:, payload:)
              RpcResponse.ok(variant, payload).with_id(req.id)
            in TransportOutcome(status:, message:)
              RpcResponse.transport_error(status, message).with_id(req.id)
            end
          end
          @carrier.send_frame(resp.encode)
          true
        end
      end
    end
  end
end
