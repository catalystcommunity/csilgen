# frozen_string_literal: true

require_relative "test_helper"

# Verify the Ruby reference library against the checked-in conformance vectors. This
# both guards the encoders/decoders against drift and demonstrates the contract every
# language's reference library follows: reconstruct each vector's envelope from its
# language-neutral `input`, assert encode -> `hex`, and assert decode(`hex`) -> the same
# envelope. Ported from transports/python/tests/test_conformance.py.
module Csilgen
  module Transport
    class RpcVectorsTest < Minitest::Test
      def test_rpc_vectors
        load_vectors("rpc.json").each do |vec|
          name = vec["name"]
          inp = vec["input"]
          expected = vec["hex"]
          case inp["kind"]
          when "request"
            env = RPC::RpcRequest.new(
              service: inp["service"],
              op: inp["op"],
              id: inp["id"],
              payload: unhex(inp["payload_hex"]),
              auth: inp["auth"]
            )
            encoded = env.encode
            assert_equal env, RPC::RpcRequest.decode(encoded), "decode #{name}"
          when "response"
            env = RPC::RpcResponse.new(
              id: inp["id"],
              status: Status.from_code(inp["status"]),
              variant: inp["variant"],
              error: inp["error"],
              payload: unhex(inp["payload_hex"])
            )
            encoded = env.encode
            assert_equal env, RPC::RpcResponse.decode(encoded), "decode #{name}"
          when "push"
            env = RPC::RpcPush.new(
              service: inp["service"],
              event: inp["event"],
              payload: unhex(inp["payload_hex"])
            )
            encoded = env.encode
            assert_equal env, RPC::RpcPush.decode(encoded), "decode #{name}"
          else
            flunk "unknown rpc kind #{inp["kind"]}"
          end
          assert_equal expected, to_hex(encoded), "encode #{name}"
        end
      end
    end

    class EventsVectorsTest < Minitest::Test
      def test_events_vectors
        load_vectors("events.json").each do |vec|
          name = vec["name"]
          inp = vec["input"]
          expected = vec["hex"]
          control = inp["control"]
          if control
            encoded = encode_control(control, inp, name)
            assert_equal expected, to_hex(encoded), "encode #{name}"
            next
          end

          profile = Events::Profile.parse(inp["profile"])
          payload = unhex(inp["payload_hex"])
          env =
            if profile == Events::Profile::VERBOSE
              Events::Event.verbose(inp["service"], inp["event"], payload)
            else
              Events::Event.compact(inp["service_ord"], inp["op_ord"], payload)
            end
          env = env.with_id(inp["id"]) unless inp["id"].nil?
          encoded = env.encode(profile)
          assert_equal expected, to_hex(encoded), "encode #{name}"
          assert_equal env, Events::Event.decode(encoded, profile), "decode #{name}"
        end
      end

      private

      def encode_control(control, inp, name)
        case control
        when "hello"
          Events::Hello.new(
            versions: inp["versions"], profiles: inp["profiles"],
            service: inp["service"], auth: inp["auth"]
          ).encode
        when "hello_ack"
          Events::HelloAck.new(v: inp["v"], profile: inp["profile"], session: inp["session"]).encode
        when "ping"
          Events::Heartbeat.new(nonce: inp["nonce"], at: inp["at"]).encode
        when "close"
          Events::Close.new(status: Status.from_code(inp["status"]), reason: inp["reason"]).encode
        else
          flunk "unknown control #{control} in #{name}"
        end
      end
    end

    class DatagramVectorsTest < Minitest::Test
      def test_datagram_vectors
        load_vectors("datagrams.json").each do |vec|
          name = vec["name"]
          inp = vec["input"]
          expected = vec["hex"]
          case inp["profile"]
          when "cbor-array"
            env = Datagrams::Datagram.new(
              op_ord: inp["op_ord"], seq: inp["seq"], payload: unhex(inp["payload_hex"])
            )
            encoded = env.encode
            assert_equal expected, to_hex(encoded), "encode #{name}"
            assert_equal env, Datagrams::Datagram.decode(encoded), "decode #{name}"
          when "compact-header"
            env = Datagrams::CompactDatagram.new(
              op_ord: inp["op_ord"], seq: inp["seq"],
              epoch: inp["epoch"], body: unhex(inp["body_hex"])
            )
            encoded = env.encode
            assert_equal expected, to_hex(encoded), "encode #{name}"
            assert_equal env, Datagrams::CompactDatagram.decode(encoded), "decode #{name}"
          else
            flunk "unknown datagram profile #{inp["profile"]}"
          end
        end
      end
    end
  end
end
