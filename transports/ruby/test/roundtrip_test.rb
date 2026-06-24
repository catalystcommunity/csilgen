# frozen_string_literal: true

require_relative "test_helper"
require "stringio"

# Round-trip and behavioral tests over the loopback carrier, mirroring the Rust and
# Python reference suites.
module Csilgen
  module Transport
    class RpcRoundTripTest < Minitest::Test
      def test_request_round_trip
        req = RPC::RpcRequest.new(service: "Attestation", op: "deposit-claim", payload: "\xA0".b).with_id(7)
        assert_equal req, RPC::RpcRequest.decode(req.encode)
      end

      def test_response_success_and_error
        ok = RPC::RpcResponse.ok("DepositClaimResponse", "\xA0".b).with_id(7)
        decoded = RPC::RpcResponse.decode(ok.encode)
        assert_equal Status.new(code: 0), decoded.status
        assert_equal "DepositClaimResponse", decoded.variant
        assert_same decoded, decoded.into_transport_error

        err = RPC::RpcResponse.transport_error(Status.new(code: 2), "no such op")
        decoded = RPC::RpcResponse.decode(err.encode)
        assert_equal Status.new(code: 2), decoded.status
        assert_raises(StatusError) { decoded.into_transport_error }
      end

      def test_application_error_is_status_ok
        # An application error rides as a typed variant with transport status 0.
        app_err = RPC::RpcResponse.ok("ServiceError", "\xA1\x00\x01".b)
        decoded = RPC::RpcResponse.decode(app_err.encode)
        assert_equal Status.new(code: 0), decoded.status
        assert_equal "ServiceError", decoded.variant
        assert_same decoded, decoded.into_transport_error
      end

      def test_client_server_over_loopback
        req = RPC::RpcRequest.new(service: "Echo", op: "say", payload: "chi".b).with_id(1)
        carrier = LoopbackFrameCarrier.new
        carrier.push_inbound(req.encode)
        server = RPC::Server.new(carrier)
        served = server.serve_one(->(r) { RPC::Reply.new(variant: "SayResponse", payload: r.payload) })
        assert served
        out = carrier.take_outbound
        refute_nil out
        resp = RPC::RpcResponse.decode(out)
        assert_equal "SayResponse", resp.variant
        assert_equal req.payload, resp.payload
        assert_equal 1, resp.id
      end

      def test_client_call_round_trip
        carrier = LoopbackFrameCarrier.new
        carrier.push_inbound(RPC::RpcResponse.ok("R", "\xA0".b).with_id(1).encode)
        client = RPC::Client.new(carrier, multiplexed: true)
        resp = client.call("S", "op", "\xA0".b)
        assert_equal "R", resp.variant
        sent = RPC::RpcRequest.decode(carrier.take_outbound)
        assert_equal 1, sent.id
      end

      def test_client_call_surfaces_transport_status
        carrier = LoopbackFrameCarrier.new
        carrier.push_inbound(RPC::RpcResponse.transport_error(Status.new(code: 2), "no such op").with_id(1).encode)
        client = RPC::Client.new(carrier, multiplexed: true)
        err = assert_raises(StatusError) { client.call("S", "op", "\xA0".b) }
        assert_equal 2, err.code
      end

      def test_version_mismatch_rejected
        bad = Conventions.canon_map([
          ["v", 2],
          ["service", "S"],
          ["op", "o"],
          ["payload", Conventions.tag24("\xA0".b)]
        ])
        assert_raises(UnsupportedVersionError) { RPC::RpcRequest.decode(Conventions.encode_value(bad)) }
      end
    end

    class EventsRoundTripTest < Minitest::Test
      def test_verbose_round_trip
        ev = Events::Event.verbose("World", "chat", "\x60".b).with_id(3)
        assert_equal ev, Events::Event.decode(ev.encode(Events::Profile::VERBOSE), Events::Profile::VERBOSE)
      end

      def test_compact_round_trip_both_shapes
        fire = Events::Event.compact(1, 0, "\x60".b)
        assert_equal fire, Events::Event.decode(fire.encode(Events::Profile::COMPACT), Events::Profile::COMPACT)

        corr = Events::Event.compact(1, 2, "\x60".b).with_id(42)
        decoded = Events::Event.decode(corr.encode(Events::Profile::COMPACT), Events::Profile::COMPACT)
        assert_equal 42, decoded.id
        assert_equal corr, decoded
      end

      def test_hello_negotiation
        hello = Events::Hello.new(versions: [1], profiles: %w[compact verbose], service: "World", auth: nil)
        round_tripped = Events::Hello.decode(hello.encode)
        assert_equal hello, round_tripped
        chosen = round_tripped.negotiate([Events::Profile::VERBOSE, Events::Profile::COMPACT])
        assert_equal [1, Events::Profile::COMPACT], chosen
      end

      def test_hello_negotiation_fails_on_version
        hello = Events::Hello.new(versions: [99], profiles: ["verbose"], service: nil, auth: nil)
        assert_nil hello.negotiate([Events::Profile::VERBOSE])
      end

      def test_control_payload_round_trips
        ack = Events::HelloAck.new(v: 1, profile: "compact", session: "s1")
        assert_equal ack, Events::HelloAck.decode(ack.encode)
        beat = Events::Heartbeat.new(nonce: 9)
        assert_equal beat, Events::Heartbeat.decode(beat.encode)
        close = Events::Close.new(status: Status.new(code: 5), reason: "bad v")
        assert_equal close, Events::Close.decode(close.encode)
      end
    end

    class DatagramsRoundTripTest < Minitest::Test
      def test_cbor_array_round_trip
        dg = Datagrams::Datagram.new(op_ord: 0, seq: 5, payload: "\x60".b)
        assert_equal dg, Datagrams::Datagram.decode(dg.encode)
        three = Conventions.encode_value([1, 0, Conventions.tag24("\x60".b)])
        assert_raises(MalformedError) { Datagrams::Datagram.decode(three) }
      end

      def test_compact_header_round_trip
        dg = Datagrams::CompactDatagram.new(op_ord: 1, seq: 0x1234, body: "\x01\x02\x03".b)
        encoded = dg.encode
        assert_equal 1, encoded.getbyte(0) >> 4
        assert_equal dg, Datagrams::CompactDatagram.decode(encoded)

        with_epoch = Datagrams::CompactDatagram.new(op_ord: 2, seq: 7, body: "\x09".b).with_epoch(4)
        decoded = Datagrams::CompactDatagram.decode(with_epoch.encode)
        assert_equal 4, decoded.epoch
        assert_equal with_epoch, decoded
      end

      def test_seq_tracker_detects_loss_reorder_restart
        t = Datagrams::SeqTracker.new
        assert_equal Datagrams.first, t.observe(1, 0)
        assert_equal Datagrams.advanced(0), t.observe(2, 0)
        assert_equal Datagrams.advanced(2), t.observe(5, 0)
        assert_equal Datagrams.late_or_duplicate, t.observe(3, 0)
        assert_equal Datagrams.restart, t.observe(1, 1)
      end

      def test_seq_tracker_treats_unsequenced_as_advanced
        t = Datagrams::SeqTracker.new
        assert_equal Datagrams.advanced(0), t.observe(0, nil)
        assert_equal Datagrams.advanced(0), t.observe(0, nil)
        assert_equal Datagrams.advanced(0), t.observe(0, nil)
      end
    end

    class FramingTest < Minitest::Test
      def test_length_prefix_round_trips_over_loopback
        c = LoopbackFrameCarrier.new
        c.send_frame("\x01\x02\x03".b)
        framed = c.take_outbound
        c.push_inbound(framed)
        assert_equal "\x01\x02\x03".b, c.recv_frame
      end

      def test_stream_carrier_round_trip
        buf = StringIO.new("".b)
        StreamCarrier.new(buf).send_frame("\x09\x08\x07".b)
        buf.rewind
        receiver = StreamCarrier.new(buf)
        assert_equal "\x09\x08\x07".b, receiver.recv_frame
        assert_nil receiver.recv_frame
      end

      def test_stream_carrier_rejects_oversize_on_send
        carrier = StreamCarrier.new(StringIO.new("".b), max_frame: 4)
        assert_raises(FrameTooLargeError) { carrier.send_frame("\x00\x00\x00\x00\x00".b) }
      end

      def test_stream_carrier_rejects_oversize_length_prefix
        buf = StringIO.new([0xFFFFFFFF].pack("N"))
        carrier = StreamCarrier.new(buf, max_frame: 16)
        assert_raises(FrameTooLargeError) { carrier.recv_frame }
      end
    end

    class CborTest < Minitest::Test
      def test_canonical_map_key_ordering
        a = CBOR.encode({"service" => "x", "v" => 1, "op" => "y"})
        b = CBOR.encode({"v" => 1, "op" => "y", "service" => "x"})
        assert_equal a, b
        assert_equal({"v" => 1, "op" => "y", "service" => "x"}, CBOR.decode(a))
      end

      def test_negative_integers
        [-1, -24, -256, -65_536, -(2**32)].each do |n|
          assert_equal n, CBOR.decode(CBOR.encode(n))
        end
      end

      def test_integer_width_boundaries
        [23, 24, 255, 256, 65_535, 65_536, 2**32 - 1, 2**32, 2**64 - 1].each do |n|
          assert_equal n, CBOR.decode(CBOR.encode(n))
        end
      end

      def test_multibyte_text_uses_byte_lengths
        ["café", "日本"].each do |s|
          assert_equal s, CBOR.decode(CBOR.encode(s))
        end
      end

      def test_tag24_round_trip
        wrapped = Conventions.tag24("\xA0".b)
        decoded = CBOR.decode(CBOR.encode(wrapped))
        assert_equal 24, decoded.tag
        assert_equal "\xA0".b, decoded.value
      end

      def test_trailing_bytes_rejected
        assert_raises(CBOR::CBORError) { CBOR.decode(CBOR.encode(1) + "\x00".b) }
      end

      def test_indefinite_length_rejected
        # Major-type-2 with additional info 31 (indefinite) is forbidden in an envelope.
        assert_raises(CBOR::CBORError) { CBOR.decode("\x5f".b) }
      end
    end
  end
end
