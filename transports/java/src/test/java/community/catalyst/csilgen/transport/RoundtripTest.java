// Round-trip unit tests mirroring the Python/Rust library tests: encode→decode equality,
// client/server over the loopback carrier, transport-status surfacing, profile negotiation,
// the sequence tracker, stream framing with the frame guard, and codec edge cases. Lives in
// the same package so it can exercise the package-private Cbor/Conventions internals.
package community.catalyst.csilgen.transport;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.util.List;
import org.junit.jupiter.api.Test;

class RoundtripTest {

    @Test
    void rpcRequestRoundTrip() {
        Rpc.Request req = Rpc.Request.of("Attestation", "deposit-claim", new byte[] {(byte) 0xA0})
                .withId(7);
        assertEquals(req, Rpc.Request.decode(req.encode()));
    }

    @Test
    void rpcResponseSuccessAndError() {
        Rpc.Response ok = Rpc.Response.ok("DepositClaimResponse", new byte[] {(byte) 0xA0})
                .withId(7L);
        Rpc.Response okDec = Rpc.Response.decode(ok.encode());
        assertEquals(Status.OK, okDec.status());
        assertEquals("DepositClaimResponse", okDec.variant());
        assertNull(okDec.asTransportError());

        Rpc.Response err = Rpc.Response.transportError(Status.UNKNOWN_SERVICE_OR_OP, "no such op");
        Rpc.Response errDec = Rpc.Response.decode(err.encode());
        assertEquals(Status.UNKNOWN_SERVICE_OR_OP, errDec.status());
        assertEquals(2, errDec.asTransportError().status.code());
        // A non-zero status carries a present-but-empty tag-24 payload, never an absent field.
        assertArrayEquals(new byte[0], errDec.payload());
    }

    @Test
    void applicationErrorIsStatusOk() {
        // An application error rides as a typed variant with transport status 0.
        Rpc.Response appErr = Rpc.Response.ok("ServiceError", new byte[] {(byte) 0xA1, 0, 1});
        Rpc.Response dec = Rpc.Response.decode(appErr.encode());
        assertEquals(Status.OK, dec.status());
        assertEquals("ServiceError", dec.variant());
        assertNull(dec.asTransportError());
    }

    @Test
    void clientServerOverLoopback() throws IOException {
        Rpc.Request req = Rpc.Request.of("Echo", "say", new byte[] {0x63, 'h', 'i'}).withId(1);
        Carriers.LoopbackFrameCarrier carrier = new Carriers.LoopbackFrameCarrier();
        carrier.pushInbound(req.encode());
        Rpc.Server server = new Rpc.Server(carrier);
        boolean served = server.serveOne(r -> Rpc.HandlerOutcome.reply("SayResponse", r.payload()));
        assertTrue(served);
        Rpc.Response resp = Rpc.Response.decode(carrier.takeOutbound());
        assertEquals("SayResponse", resp.variant());
        assertArrayEquals(req.payload(), resp.payload());
        assertEquals(1L, resp.id());
    }

    @Test
    void clientCallRoundTrip() throws IOException {
        Carriers.LoopbackFrameCarrier carrier = new Carriers.LoopbackFrameCarrier();
        carrier.pushInbound(Rpc.Response.ok("R", new byte[] {(byte) 0xA0}).withId(1L).encode());
        Rpc.Client client = new Rpc.Client(carrier, true);
        Rpc.Response resp = client.call("S", "op", new byte[] {(byte) 0xA0}, null);
        assertEquals("R", resp.variant());
        Rpc.Request sent = Rpc.Request.decode(carrier.takeOutbound());
        assertEquals(1L, sent.id());
    }

    @Test
    void clientCallSurfacesTransportStatus() {
        Carriers.LoopbackFrameCarrier carrier = new Carriers.LoopbackFrameCarrier();
        carrier.pushInbound(Rpc.Response.transportError(Status.UNKNOWN_SERVICE_OR_OP, "no such op")
                .withId(1L).encode());
        Rpc.Client client = new Rpc.Client(carrier, true);
        StatusException ex = assertThrows(StatusException.class,
                () -> client.call("S", "op", new byte[] {(byte) 0xA0}, null));
        assertEquals(2, ex.status.code());
    }

    @Test
    void versionMismatchRejected() {
        byte[] bad = Conventions.encodeMap(List.of(
                Conventions.entry("v", new CUint(2)),
                Conventions.textEntry("service", "S"),
                Conventions.textEntry("op", "o"),
                Conventions.entry("payload", Conventions.tag24(new byte[] {(byte) 0xA0}))));
        assertThrows(UnsupportedVersionException.class, () -> Rpc.Request.decode(bad));
    }

    @Test
    void verboseEventRoundTrip() {
        Events.Event ev = Events.Event.verbose("World", "chat", new byte[] {0x60}).withId(3);
        assertEquals(ev, Events.Event.decode(ev.encode(Events.Profile.VERBOSE),
                Events.Profile.VERBOSE));
    }

    @Test
    void compactEventRoundTripBothShapes() {
        Events.Event fire = Events.Event.compact(1, 0, new byte[] {0x60});
        assertEquals(fire, Events.Event.decode(fire.encode(Events.Profile.COMPACT),
                Events.Profile.COMPACT));

        Events.Event corr = Events.Event.compact(1, 2, new byte[] {0x60}).withId(42);
        Events.Event dec = Events.Event.decode(corr.encode(Events.Profile.COMPACT),
                Events.Profile.COMPACT);
        assertEquals(42L, dec.id());
        assertEquals(corr, dec);
    }

    @Test
    void helloNegotiation() {
        Events.Hello hello = new Events.Hello(List.of(1L), List.of("compact", "verbose"), "World",
                null);
        Events.Hello round = Events.Hello.decode(hello.encode());
        assertEquals(hello, round);
        Events.Negotiation chosen = round.negotiate(
                List.of(Events.Profile.VERBOSE, Events.Profile.COMPACT));
        assertEquals(new Events.Negotiation(1, Events.Profile.COMPACT), chosen);
    }

    @Test
    void helloNegotiationFailsOnVersion() {
        Events.Hello hello = new Events.Hello(List.of(99L), List.of("verbose"), null, null);
        assertNull(hello.negotiate(List.of(Events.Profile.VERBOSE)));
    }

    @Test
    void controlPayloadRoundTrips() {
        Events.HelloAck ack = new Events.HelloAck(1, "compact", "s1");
        assertEquals(ack, Events.HelloAck.decode(ack.encode()));
        Events.Heartbeat beat = new Events.Heartbeat(9, null);
        assertEquals(beat, Events.Heartbeat.decode(beat.encode()));
        Events.Close close = new Events.Close(Status.VERSION_UNSUPPORTED, "bad v");
        assertEquals(close, Events.Close.decode(close.encode()));
    }

    @Test
    void cborArrayDatagramRoundTrip() {
        Datagrams.Datagram dg = new Datagrams.Datagram(0, 5, new byte[] {0x60});
        assertEquals(dg, Datagrams.Datagram.decode(dg.encode()));
        // Wrong arity rejected.
        byte[] three = Cbor.encode(new CArray(List.of(new CUint(1), new CUint(0),
                Conventions.tag24(new byte[] {0x60}))));
        assertThrows(MalformedException.class, () -> Datagrams.Datagram.decode(three));
    }

    @Test
    void compactHeaderDatagramRoundTrip() {
        Datagrams.CompactDatagram dg = Datagrams.CompactDatagram.of(1, 0x1234,
                new byte[] {1, 2, 3});
        byte[] encoded = dg.encode();
        assertEquals(1, (encoded[0] & 0xFF) >> 4);
        assertEquals(dg, Datagrams.CompactDatagram.decode(encoded));

        Datagrams.CompactDatagram withEpoch = Datagrams.CompactDatagram.of(2, 7, new byte[] {9})
                .withEpoch(4);
        Datagrams.CompactDatagram dec = Datagrams.CompactDatagram.decode(withEpoch.encode());
        assertEquals(4, dec.epoch());
        assertEquals(withEpoch, dec);
    }

    @Test
    void seqTrackerDetectsLossReorderRestart() {
        Datagrams.SeqTracker t = new Datagrams.SeqTracker();
        assertEquals(new Datagrams.SeqEvent(Datagrams.SeqEventKind.FIRST, 0), t.observe(1, 0));
        assertEquals(new Datagrams.SeqEvent(Datagrams.SeqEventKind.ADVANCED, 0), t.observe(2, 0));
        // lost 3, 4
        assertEquals(new Datagrams.SeqEvent(Datagrams.SeqEventKind.ADVANCED, 2), t.observe(5, 0));
        assertEquals(new Datagrams.SeqEvent(Datagrams.SeqEventKind.LATE_OR_DUPLICATE, 0),
                t.observe(3, 0));
        // epoch bumped
        assertEquals(new Datagrams.SeqEvent(Datagrams.SeqEventKind.RESTART, 0), t.observe(1, 1));
    }

    @Test
    void seqTrackerTreatsUnsequencedAsAdvanced() {
        Datagrams.SeqTracker t = new Datagrams.SeqTracker();
        for (int i = 0; i < 3; i++) {
            assertEquals(new Datagrams.SeqEvent(Datagrams.SeqEventKind.ADVANCED, 0),
                    t.observe(0, null));
        }
    }

    @Test
    void streamCarrierRoundTrip() throws IOException {
        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        Carriers.StreamCarrier sender = new Carriers.StreamCarrier(
                new ByteArrayInputStream(new byte[0]), sink);
        sender.sendFrame(new byte[] {9, 8, 7});
        Carriers.StreamCarrier receiver = new Carriers.StreamCarrier(
                new ByteArrayInputStream(sink.toByteArray()), new ByteArrayOutputStream());
        assertArrayEquals(new byte[] {9, 8, 7}, receiver.recvFrame());
        // Clean EOF returns null.
        assertNull(receiver.recvFrame());
    }

    @Test
    void streamCarrierRejectsOversizeOnSend() {
        Carriers.StreamCarrier carrier = new Carriers.StreamCarrier(
                new ByteArrayInputStream(new byte[0]), new ByteArrayOutputStream(), 4);
        assertThrows(FrameTooLargeException.class, () -> carrier.sendFrame(new byte[5]));
    }

    @Test
    void streamCarrierRejectsOversizeLengthPrefix() {
        // A hostile 0xffffffff length prefix must be rejected before allocation.
        byte[] hostile = {(byte) 0xFF, (byte) 0xFF, (byte) 0xFF, (byte) 0xFF};
        Carriers.StreamCarrier carrier = new Carriers.StreamCarrier(
                new ByteArrayInputStream(hostile), new ByteArrayOutputStream(), 16);
        assertThrows(FrameTooLargeException.class, carrier::recvFrame);
    }

    @Test
    void negativeIntegers() {
        for (long n : new long[] {-1, -24, -256, -65536, -(1L << 32)}) {
            byte[] enc = Cbor.encode(new CInt(n));
            CborValue dec = Cbor.decodeEnvelope(enc);
            assertEquals(new CInt(n), dec);
        }
    }

    @Test
    void tag24RoundTrip() {
        CborValue wrapped = Conventions.tag24(new byte[] {(byte) 0xA0});
        CborValue decoded = Cbor.decodeEnvelope(Cbor.encode(wrapped));
        assertArrayEquals(new byte[] {(byte) 0xA0}, Conventions.untag24(decoded));
    }

    @Test
    void trailingBytesRejected() {
        byte[] withTrailer = {0x01, 0x00};
        assertThrows(DecodeException.class, () -> Cbor.decodeEnvelope(withTrailer));
    }

    @Test
    void decoderRejectsHostileLengthsDepthAndText() {
        byte[][] bad = {
            {(byte) 0x9B, 0, 0, 1, 0, 0, 0, 0, 0},
            {(byte) 0xBB, 0, 0, 1, 0, 0, 0, 0, 0},
            {0x61, (byte) 0xFF}
        };
        for (byte[] payload : bad) {
            assertThrows(DecodeException.class, () -> Cbor.decodeEnvelope(payload));
        }

        byte[] tooDeep = new byte[66];
        java.util.Arrays.fill(tooDeep, 0, 65, (byte) 0xC0);
        assertThrows(DecodeException.class, () -> Cbor.decodeEnvelope(tooDeep));

        byte[] allowed = new byte[65];
        java.util.Arrays.fill(allowed, 0, 64, (byte) 0xC0);
        Cbor.decodeEnvelope(allowed);
    }

    @Test
    void canonicalMapKeyOrdering() {
        // Keys come out in deterministic order regardless of insertion order.
        byte[] a = Conventions.encodeMap(List.of(
                Conventions.textEntry("service", "x"),
                Conventions.entry("v", new CUint(1)),
                Conventions.textEntry("op", "y")));
        byte[] b = Conventions.encodeMap(List.of(
                Conventions.entry("v", new CUint(1)),
                Conventions.textEntry("op", "y"),
                Conventions.textEntry("service", "x")));
        assertArrayEquals(a, b);
    }
}
