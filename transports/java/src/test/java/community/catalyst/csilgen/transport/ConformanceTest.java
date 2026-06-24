// Verify the Java transport library against the checked-in conformance vectors.
//
// This both guards the encoders/decoders against drift and demonstrates the contract every
// reference library follows: reconstruct each vector's envelope from its language-neutral
// `input`, assert encode → `hex`, and assert decode(`hex`) → the same envelope. Ported from
// transports/go/conformance_test.go and transports/rust/tests/conformance.rs.
package community.catalyst.csilgen.transport;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.MethodSource;

class ConformanceTest {

    static List<Vectors.Case> rpcVectors() {
        return Vectors.load("rpc.json");
    }

    static List<Vectors.Case> eventVectors() {
        return Vectors.load("events.json");
    }

    static List<Vectors.Case> datagramVectors() {
        return Vectors.load("datagrams.json");
    }

    @ParameterizedTest(name = "rpc/{0}")
    @MethodSource("rpcVectors")
    void rpc(Vectors.Case vec) {
        Map<String, Object> in = vec.input();
        byte[] actual;
        switch (Vectors.reqStr(in, "kind")) {
            case "request" -> {
                Rpc.Request r = new Rpc.Request(
                        Vectors.reqStr(in, "service"),
                        Vectors.reqStr(in, "op"),
                        Vectors.optLong(in, "id"),
                        Vectors.hex(Vectors.reqStr(in, "payload_hex")),
                        Vectors.optStr(in, "auth"));
                actual = r.encode();
                assertEquals(r, Rpc.Request.decode(actual), "decode mismatch " + vec.name());
            }
            case "response" -> {
                Rpc.Response r = new Rpc.Response(
                        Vectors.optLong(in, "id"),
                        Status.fromCode(Vectors.reqLong(in, "status")),
                        Vectors.optStr(in, "variant"),
                        Vectors.optStr(in, "error"),
                        Vectors.hex(Vectors.reqStr(in, "payload_hex")));
                actual = r.encode();
                assertEquals(r, Rpc.Response.decode(actual), "decode mismatch " + vec.name());
            }
            case "push" -> {
                Rpc.Push p = new Rpc.Push(
                        Vectors.reqStr(in, "service"),
                        Vectors.reqStr(in, "event"),
                        Vectors.hex(Vectors.reqStr(in, "payload_hex")));
                actual = p.encode();
                assertEquals(p, Rpc.Push.decode(actual), "decode mismatch " + vec.name());
            }
            default -> throw new IllegalArgumentException("unknown rpc kind");
        }
        assertEquals(vec.hex(), Vectors.toHex(actual), "encode mismatch " + vec.name());
    }

    @ParameterizedTest(name = "events/{0}")
    @MethodSource("eventVectors")
    void events(Vectors.Case vec) {
        Map<String, Object> in = vec.input();
        String control = Vectors.optStr(in, "control");
        if (control != null) {
            byte[] bytes = encodeControl(control, in);
            assertEquals(vec.hex(), Vectors.toHex(bytes), "encode mismatch " + vec.name());
            return;
        }

        Events.Profile profile = Events.Profile.parse(Vectors.reqStr(in, "profile"));
        byte[] payload = Vectors.hex(Vectors.reqStr(in, "payload_hex"));
        Events.Event ev;
        if (profile == Events.Profile.VERBOSE) {
            ev = new Events.Event(Vectors.optStr(in, "service"), null, Vectors.reqStr(in, "event"),
                    null, Vectors.optLong(in, "id"), payload);
        } else {
            Long id = Vectors.optLong(in, "id");
            ev = new Events.Event(null, Vectors.reqLong(in, "service_ord"), null,
                    Vectors.reqLong(in, "op_ord"), id, payload);
        }
        byte[] actual = ev.encode(profile);
        assertEquals(vec.hex(), Vectors.toHex(actual), "encode mismatch " + vec.name());
        assertEquals(ev, Events.Event.decode(actual, profile), "decode mismatch " + vec.name());
    }

    @SuppressWarnings("unchecked")
    private static byte[] encodeControl(String control, Map<String, Object> in) {
        switch (control) {
            case "hello" -> {
                List<Long> versions = new ArrayList<>();
                for (Object x : (List<Object>) in.get("versions")) {
                    versions.add((long) (double) (Double) x);
                }
                List<String> profiles = new ArrayList<>();
                for (Object x : (List<Object>) in.get("profiles")) {
                    profiles.add((String) x);
                }
                return new Events.Hello(versions, profiles, Vectors.optStr(in, "service"),
                        Vectors.optStr(in, "auth")).encode();
            }
            case "hello_ack" -> {
                return new Events.HelloAck(Vectors.reqLong(in, "v"), Vectors.reqStr(in, "profile"),
                        Vectors.optStr(in, "session")).encode();
            }
            case "ping" -> {
                return new Events.Heartbeat(Vectors.reqLong(in, "nonce"), Vectors.optLong(in, "at"))
                        .encode();
            }
            case "close" -> {
                return new Events.Close(Status.fromCode(Vectors.reqLong(in, "status")),
                        Vectors.optStr(in, "reason")).encode();
            }
            default -> throw new IllegalArgumentException("unknown control " + control);
        }
    }

    @ParameterizedTest(name = "datagrams/{0}")
    @MethodSource("datagramVectors")
    void datagrams(Vectors.Case vec) {
        Map<String, Object> in = vec.input();
        switch (Vectors.reqStr(in, "profile")) {
            case "cbor-array" -> {
                Datagrams.Datagram d = new Datagrams.Datagram(Vectors.reqLong(in, "op_ord"),
                        Vectors.reqLong(in, "seq"), Vectors.hex(Vectors.reqStr(in, "payload_hex")));
                byte[] actual = d.encode();
                assertEquals(vec.hex(), Vectors.toHex(actual), "encode mismatch " + vec.name());
                assertEquals(d, Datagrams.Datagram.decode(actual),
                        "decode mismatch " + vec.name());
            }
            case "compact-header" -> {
                Datagrams.CompactDatagram d = Datagrams.CompactDatagram.of(
                        (int) Vectors.reqLong(in, "op_ord"),
                        (int) Vectors.reqLong(in, "seq"),
                        Vectors.hex(Vectors.reqStr(in, "body_hex")));
                Long epoch = Vectors.optLong(in, "epoch");
                if (epoch != null) {
                    d = d.withEpoch(epoch.intValue());
                }
                byte[] actual = d.encode();
                assertEquals(vec.hex(), Vectors.toHex(actual), "encode mismatch " + vec.name());
                assertEquals(d, Datagrams.CompactDatagram.decode(actual),
                        "decode mismatch " + vec.name());
            }
            default -> throw new IllegalArgumentException("unknown datagram profile");
        }
    }
}
