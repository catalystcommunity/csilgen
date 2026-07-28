package interop;

// Java interop harness — mirrors the Rust reference harness (tests/interop/harness/rust).
// Runs as a server (binds a loopback TCP/UDP port, serves one transport) or a client
// (connects, runs the case battery for one transport, prints one JSON result line). The
// wire is owned by the generated codec (csilgen.generated.CsilCbor) and the transport
// library (community.catalyst.csilgen.transport); this harness only choreographs them.

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.math.BigDecimal;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.StandardSocketOptions;
import java.nio.channels.Channels;
import java.nio.channels.ServerSocketChannel;
import java.nio.channels.SocketChannel;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import community.catalyst.csilgen.transport.Carriers;
import community.catalyst.csilgen.transport.Events;
import community.catalyst.csilgen.transport.Datagrams.Datagram;
import community.catalyst.csilgen.transport.FrameCarrier;
import community.catalyst.csilgen.transport.Rpc;
import community.catalyst.csilgen.transport.Status;
import community.catalyst.csilgen.transport.StatusException;

import csilgen.generated.ClientException;
import csilgen.generated.Codec;
import csilgen.generated.Collections;
import csilgen.generated.Color;
import csilgen.generated.Constrained;
import csilgen.generated.CsilCbor;
import csilgen.generated.EchoNestedResult;
import csilgen.generated.IdOrName;
import csilgen.generated.Interop;
import csilgen.generated.InteropClient;
import csilgen.generated.InteropRouter;
import csilgen.generated.Level;
import csilgen.generated.MixedStatus;
import csilgen.generated.Nested;
import csilgen.generated.OptBytes;
import csilgen.generated.Priority;
import csilgen.generated.Scalars;
import csilgen.generated.ScalarsNote;
import csilgen.generated.Season;
import csilgen.generated.ServiceError;
import csilgen.generated.ShipMode;
import csilgen.generated.Tick;
import csilgen.generated.Transport;

public final class Main {
    private static final String SERVICE = "Interop";
    private static final long TICKS = 3;

    private static final long OP_ECHO_SCALARS = 0;
    private static final long OP_ECHO_COLLECTIONS = 1;
    private static final long OP_ERROR_SENTINEL = 255;

    // -----------------------------------------------------------------------
    // Fixed language-neutral test vectors (see tests/interop/README.md).
    // -----------------------------------------------------------------------

    private static Instant ts() {
        return Instant.parse("2026-06-29T12:34:56Z");
    }

    private static Scalars scalarsOk() {
        // mixed-union coverage: a value equal to a literal arm must wire as
        // [1,"pending"]; a value with no literal match must wire as [0,...].
        return new Scalars(
            -42L, 42L, -7L, 3.5, "héllo 世界",
            new byte[] {0x01, 0x02, (byte) 0xf0, (byte) 0xff},
            true, ts(), new BigDecimal("123.45"),
            new MixedStatus("pending"), new MixedStatus("unlisted"),
            // inline mixed choice (hoisted; literal arm match).
            new ScalarsNote("info"),
            // inline all-literal choice (not hoisted).
            "medium",
            // named enum with a trailing `.default`-constrained last arm.
            new Level("high"),
            // named enum assembled via base rule + `/=` extension.
            new Season("autumn"),
            // named all-literal enum with mixed literal kinds (text + int arms);
            // still gets a wrapper record for a NAMED reference (unlike Java's
            // un-hoisted inline choices, which render bare with no wrapper).
            new ShipMode("ground"),
            new ShipMode(2L));
    }

    private static Collections collectionsOk() {
        Map<String, Long> scores = new LinkedHashMap<>();
        scores.put("x", 1L);
        scores.put("y", 2L);
        Map<String, CsilCbor.CborValue> extra = new LinkedHashMap<>();
        extra.put("k", new CsilCbor.CborText("v"));
        return new Collections(
            List.of("a", "b"),
            List.of(1L, 2L, 3L),
            List.of(10L, 20L),
            List.of(7L, 8L, 9L),
            scores,
            new Color("green"),
            new Color("blue"),
            new Priority(2L),
            new IdOrName(4242L),
            Arrays.<Object>asList("p", 5L),
            Arrays.<Object>asList("t", 9L, Boolean.TRUE),
            extra);
    }

    private static Constrained constrainedOk() {
        return new Constrained(
            "PRD-AB12CD", 10L, new BigDecimal("0.25"), "hunter2hunter2",
            List.of("one", "two"));
    }

    private static Nested nestedOk() {
        return new Nested(scalarsOk(), constrainedOk(), List.of(scalarsOk()));
    }

    // The invalid Constrained must be built as raw CBOR: the generated record validates
    // in its canonical constructor, so an invalid value cannot be constructed in Java.
    // These well-formed-but-invalid bytes decode fine on any server, then fail validation.
    private static byte[] constrainedBadBytes() {
        return CsilCbor.encode(new CsilCbor.CborMap(List.of(
            new CsilCbor.CborEntry(new CsilCbor.CborText("code"), new CsilCbor.CborText("bad")),
            new CsilCbor.CborEntry(new CsilCbor.CborText("qty"), new CsilCbor.CborUint(0)),
            new CsilCbor.CborEntry(new CsilCbor.CborText("rate"),
                CsilCbor.encDecimal(new BigDecimal("9.9"))),
            new CsilCbor.CborEntry(new CsilCbor.CborText("password"), new CsilCbor.CborText("x")),
            new CsilCbor.CborEntry(new CsilCbor.CborText("tags"),
                new CsilCbor.CborArray(List.of())))));
    }

    // -----------------------------------------------------------------------
    // Result accumulation + JSON output (no JSON dependency).
    // -----------------------------------------------------------------------

    private static final class Cases {
        private final String transport;
        private final List<String[]> out = new ArrayList<>();

        Cases(String transport) {
            this.transport = transport;
        }

        void pass(String name) {
            out.add(new String[] {name, "true", ""});
        }

        void fail(String name, String detail) {
            out.add(new String[] {name, "false", detail});
        }

        void check(String name, boolean cond, String detail) {
            if (cond) {
                pass(name);
            } else {
                fail(name, detail);
            }
        }

        void emit() {
            StringBuilder s = new StringBuilder();
            s.append("{\"lang\":\"java\",\"transport\":\"").append(transport).append("\",\"cases\":[");
            for (int i = 0; i < out.size(); i++) {
                String[] c = out.get(i);
                if (i > 0) {
                    s.append(',');
                }
                s.append("{\"name\":\"").append(c[0]).append("\",\"ok\":").append(c[1])
                    .append(",\"detail\":").append(jsonStr(c[2])).append('}');
            }
            s.append("]}");
            System.out.println(s);
        }
    }

    private static String jsonStr(String s) {
        StringBuilder o = new StringBuilder("\"");
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"' -> o.append("\\\"");
                case '\\' -> o.append("\\\\");
                case '\n' -> o.append("\\n");
                case '\t' -> o.append("\\t");
                case '\r' -> o.append("\\r");
                default -> {
                    if (c < 0x20) {
                        o.append(String.format("\\u%04x", (int) c));
                    } else {
                        o.append(c);
                    }
                }
            }
        }
        o.append('"');
        return o.toString();
    }

    // -----------------------------------------------------------------------
    // Server-side handler implementing the generated trait.
    // -----------------------------------------------------------------------

    private static final class Handlers implements Interop {
        @Override
        public Scalars echoScalars(Scalars req) {
            return req;
        }

        @Override
        public Collections echoCollections(Collections req) {
            return req;
        }

        @Override
        public EchoNestedResult echoNested(Nested req) {
            return new EchoNestedResult(true, req);
        }

        @Override
        public OptBytes echoOptBytes(OptBytes req) {
            return req;
        }

        @Override
        public Constrained validateConstrained(Constrained req) {
            // Decoding already ran the canonical constructor's validation; a value that
            // reaches here is valid, so echo it.
            return req;
        }

        @Override
        public void duplex(Scalars msg) {
            // The reference server has no side effect; the echo happens in the react loop.
        }
    }

    // A Codec adapter over the generated per-type CBOR codec, for the generated router.
    private static final class CborCodec implements Codec {
        @Override
        public byte[] encode(Object value) {
            if (value instanceof Tick t) {
                return CsilCbor.encodeTick(t);
            }
            if (value instanceof Scalars s) {
                return CsilCbor.encodeScalars(s);
            }
            if (value instanceof Collections c) {
                return CsilCbor.encodeCollections(c);
            }
            if (value instanceof Nested n) {
                return CsilCbor.encodeNested(n);
            }
            throw new IllegalArgumentException("no codec for " + value.getClass());
        }

        @Override
        @SuppressWarnings("unchecked")
        public <T> T decode(byte[] data, Class<T> type) {
            if (type == Scalars.class) {
                return (T) CsilCbor.decodeScalars(data);
            }
            if (type == Tick.class) {
                return (T) CsilCbor.decodeTick(data);
            }
            if (type == Collections.class) {
                return (T) CsilCbor.decodeCollections(data);
            }
            if (type == Nested.class) {
                return (T) CsilCbor.decodeNested(data);
            }
            throw new IllegalArgumentException("no codec for " + type);
        }
    }

    // A typed application error surfaced from a status-0 reply whose variant is the
    // declared `ServiceError` arm (the carrier owns this mapping, per the conventions).
    private static final class ServiceErrorException extends ClientException {
        ServiceErrorException(long code, String message) {
            super("service error code=" + code + " message=" + message);
        }
    }

    // -----------------------------------------------------------------------
    // RPC
    // -----------------------------------------------------------------------

    private static final class RpcTransport implements Transport {
        private final Rpc.Client client;

        RpcTransport(Rpc.Client client) {
            this.client = client;
        }

        @Override
        public byte[] call(String service, String op, byte[] req) throws ClientException {
            try {
                Rpc.Response resp = client.call(service, op, req, null);
                // A status-0 reply whose variant is the `ServiceError` arm is a typed
                // application error; surface it on the error path.
                if ("ServiceError".equals(resp.variant())) {
                    ServiceError se = CsilCbor.decodeServiceError(resp.payload());
                    throw new ServiceErrorException(se.code(), se.message());
                }
                return resp.payload();
            } catch (StatusException e) {
                throw new ClientException("transport status " + e.getMessage(), e);
            } catch (IOException e) {
                throw new ClientException("io: " + e.getMessage(), e);
            }
        }
    }

    private static Rpc.Handler rpcHandler(Handlers handlers) {
        return req -> {
            try {
                switch (req.op()) {
                    case "echo-scalars" -> {
                        Scalars v = CsilCbor.decodeScalars(req.payload());
                        return Rpc.HandlerOutcome.reply(
                            "Scalars", CsilCbor.encodeScalars(handlers.echoScalars(v)));
                    }
                    case "echo-collections" -> {
                        Collections v = CsilCbor.decodeCollections(req.payload());
                        return Rpc.HandlerOutcome.reply(
                            "Collections", CsilCbor.encodeCollections(handlers.echoCollections(v)));
                    }
                    case "echo-nested" -> {
                        Nested v = CsilCbor.decodeNested(req.payload());
                        return Rpc.HandlerOutcome.reply(
                            "EchoNestedResult",
                            CsilCbor.encodeEchoNestedResult(handlers.echoNested(v)));
                    }
                    case "echo-opt-bytes" -> {
                        OptBytes v = CsilCbor.decodeOptBytes(req.payload());
                        return Rpc.HandlerOutcome.reply(
                            "OptBytes", CsilCbor.encodeOptBytes(handlers.echoOptBytes(v)));
                    }
                    case "validate-constrained" -> {
                        try {
                            // decodeConstrained runs the canonical constructor's validation.
                            Constrained v = CsilCbor.decodeConstrained(req.payload());
                            return Rpc.HandlerOutcome.reply(
                                "Constrained",
                                CsilCbor.encodeConstrained(handlers.validateConstrained(v)));
                        } catch (IllegalArgumentException ve) {
                            // The typed error arm rides as a status-0 `ServiceError` variant.
                            ServiceError se = new ServiceError(422, ve.getMessage());
                            return Rpc.HandlerOutcome.reply(
                                "ServiceError", CsilCbor.encodeServiceError(se));
                        }
                    }
                    default -> {
                        return Rpc.HandlerOutcome.transport(
                            Status.UNKNOWN_SERVICE_OR_OP, "unknown op " + req.op());
                    }
                }
            } catch (CsilCbor.CsilCborException e) {
                return Rpc.HandlerOutcome.transport(Status.MALFORMED_ENVELOPE, e.getMessage());
            }
        };
    }

    private static void rpcServer(ServerSocketChannel listener) throws IOException {
        Handlers handlers = new Handlers();
        Rpc.Handler handler = rpcHandler(handlers);
        while (true) {
            SocketChannel conn = listener.accept();
            FrameCarrier carrier = channelCarrier(conn);
            Rpc.Server server = new Rpc.Server(carrier);
            try {
                while (server.serveOne(handler)) {
                    // keep serving this connection
                }
            } catch (IOException e) {
                // Client went away mid-exchange; accept the next connection.
            }
            conn.close();
        }
    }

    private static Cases rpcClient(SocketChannel conn) throws IOException {
        Cases cases = new Cases("rpc");
        RpcTransport transport = new RpcTransport(new Rpc.Client(channelCarrier(conn), false));
        InteropClient client = new InteropClient(transport);

        try {
            Scalars r = client.echoScalars(scalarsOk());
            cases.check("echo-scalars/success", r.equals(scalarsOk()), "mismatch: " + r);
        } catch (RuntimeException e) {
            cases.fail("echo-scalars/success", e.toString());
        }
        try {
            Collections r = client.echoCollections(collectionsOk());
            cases.check("echo-collections/success", r.equals(collectionsOk()), "mismatch: " + r);
        } catch (RuntimeException e) {
            cases.fail("echo-collections/success", e.toString());
        }
        try {
            EchoNestedResult r = client.echoNested(nestedOk());
            cases.check("echo-nested/success", r.ok() && r.echo().equals(nestedOk()),
                "mismatch: " + r);
        } catch (RuntimeException e) {
            cases.fail("echo-nested/success", e.toString());
        }
        try {
            Constrained r = client.validateConstrained(constrainedOk());
            cases.check("validate-constrained/success", r.equals(constrainedOk()),
                "mismatch: " + r);
        } catch (RuntimeException e) {
            cases.fail("validate-constrained/success", e.toString());
        }
        // The invalid input must surface as a service error, not a transport error.
        try {
            transport.call(SERVICE, "validate-constrained", constrainedBadBytes());
            cases.fail("validate-constrained/failure", "server accepted invalid input");
        } catch (ServiceErrorException e) {
            cases.pass("validate-constrained/failure");
        } catch (RuntimeException e) {
            cases.fail("validate-constrained/failure", "expected service error, got " + e);
        }
        // Optional-bytes presence: absent, present-empty, and present-non-empty are three
        // distinct states that must each survive the round trip unchanged. `null` is the
        // absent marker -- a zero-length byte[] is present and must not decode to null.
        try {
            OptBytes r = client.echoOptBytes(new OptBytes("absent", null));
            cases.check("opt-bytes/absent", r.payload() == null,
                "expected absent, got " + Arrays.toString(r.payload()));
        } catch (RuntimeException e) {
            cases.fail("opt-bytes/absent", e.toString());
        }
        try {
            OptBytes r = client.echoOptBytes(new OptBytes("empty", new byte[0]));
            cases.check("opt-bytes/present-empty",
                r.payload() != null && r.payload().length == 0,
                "expected present-and-empty, got " + Arrays.toString(r.payload()));
        } catch (RuntimeException e) {
            cases.fail("opt-bytes/present-empty", e.toString());
        }
        try {
            byte[] sent = new byte[] {0x01, 0x02, (byte) 0xf0, (byte) 0xff};
            OptBytes r = client.echoOptBytes(new OptBytes("full", sent));
            cases.check("opt-bytes/present-non-empty", Arrays.equals(r.payload(), sent),
                "expected 0102f0ff, got " + Arrays.toString(r.payload()));
        } catch (RuntimeException e) {
            cases.fail("opt-bytes/present-non-empty", e.toString());
        }
        return cases;
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    private static void sendEvent(FrameCarrier carrier, Events.Event ev) throws IOException {
        carrier.sendFrame(ev.encode(Events.Profile.VERBOSE));
    }

    private static void eventsServer(ServerSocketChannel listener) throws IOException {
        Handlers handlers = new Handlers();
        Codec codec = new CborCodec();
        while (true) {
            SocketChannel conn = listener.accept();
            FrameCarrier carrier = channelCarrier(conn);
            try {
                eventsServeOne(carrier, handlers, codec);
            } catch (IOException e) {
                // Connection ended; serve the next.
            }
            conn.close();
        }
    }

    private static void eventsServeOne(FrameCarrier carrier, Handlers handlers, Codec codec)
            throws IOException {
        // Handshake: read $hello, reply $hello-ack (verbose).
        byte[] frame = carrier.recvFrame();
        if (frame == null) {
            return;
        }
        Events.Event hello = Events.Event.decode(frame, Events.Profile.VERBOSE);
        if (!Events.HELLO_NAME.equals(hello.event())) {
            return;
        }
        Events.HelloAck ack = new Events.HelloAck(1, "verbose", null);
        sendEvent(carrier, Events.Event.verbose(null, Events.HELLO_ACK_NAME, ack.encode()));

        // Push N ticks (on-tick / server push).
        for (long seq = 0; seq < TICKS; seq++) {
            var em = InteropRouter.encodeInteropOnTick(codec, new Tick(seq));
            sendEvent(carrier, Events.Event.verbose(SERVICE, em.method(), em.data()));
        }

        // React loop.
        while (true) {
            byte[] f = carrier.recvFrame();
            if (f == null) {
                break;
            }
            Events.Event ev = Events.Event.decode(f, Events.Profile.VERBOSE);
            String method = ev.event() == null ? "" : ev.event();
            if (method.equals(Events.PING_NAME)) {
                sendEvent(carrier, Events.Event.verbose(null, Events.PONG_NAME, ev.payload()));
            } else if (method.equals(Events.CLOSE_NAME)) {
                break;
            } else if (method.equals("duplex")) {
                Scalars s = CsilCbor.decodeScalars(ev.payload());
                handlers.duplex(s);
                var em = InteropRouter.encodeInteropDuplex(codec, s);
                sendEvent(carrier, Events.Event.verbose(SERVICE, em.method(), em.data()));
            } else {
                // Exercise the generated router's unknown-method path.
                try {
                    InteropRouter.routeInteropChannel(handlers, codec, method, ev.payload());
                } catch (RuntimeException ignored) {
                    // expected for an unknown method
                }
                sendEvent(carrier, Events.Event.verbose(null, Events.ERROR_NAME, new byte[0]));
            }
        }
    }

    private static Cases eventsClient(SocketChannel conn) throws IOException {
        Cases cases = new Cases("events");
        FrameCarrier carrier = channelCarrier(conn);

        // Handshake.
        Events.Hello hello = new Events.Hello(List.of(1L), List.of("verbose"), SERVICE, null);
        sendEvent(carrier, Events.Event.verbose(null, Events.HELLO_NAME, hello.encode()));
        byte[] ackFrame = carrier.recvFrame();
        boolean ackOk = ackFrame != null
            && Events.HELLO_ACK_NAME.equals(
                Events.Event.decode(ackFrame, Events.Profile.VERBOSE).event());
        cases.check("events/handshake", ackOk, "no $hello-ack");

        // on-tick: expect TICKS pushes with seq 0..TICKS.
        boolean tickOk = true;
        String detail = "";
        for (long expect = 0; expect < TICKS; expect++) {
            byte[] f = carrier.recvFrame();
            if (f == null) {
                tickOk = false;
                detail = "stream closed during ticks";
                break;
            }
            Events.Event ev = Events.Event.decode(f, Events.Profile.VERBOSE);
            if (!"on-tick".equals(ev.event())) {
                tickOk = false;
                detail = "expected on-tick got " + ev.event();
                break;
            }
            Tick t = CsilCbor.decodeTick(ev.payload());
            if (t.seq() != expect) {
                tickOk = false;
                detail = "tick seq " + t.seq() + " != " + expect;
                break;
            }
        }
        cases.check("on-tick/success", tickOk, detail);

        // duplex: send Scalars, expect echo.
        var em = InteropRouter.encodeInteropDuplex(new CborCodec(), scalarsOk());
        sendEvent(carrier, Events.Event.verbose(SERVICE, em.method(), em.data()));
        byte[] df = carrier.recvFrame();
        if (df == null) {
            cases.fail("duplex/success", "no echo");
        } else {
            Events.Event ev = Events.Event.decode(df, Events.Profile.VERBOSE);
            if ("duplex".equals(ev.event())) {
                Scalars s = CsilCbor.decodeScalars(ev.payload());
                cases.check("duplex/success", s.equals(scalarsOk()), "echo mismatch");
            } else {
                cases.fail("duplex/success", "got " + ev.event());
            }
        }

        // unknown-method: send a bogus channel method, expect $error.
        sendEvent(carrier, Events.Event.verbose(SERVICE, "Bogus", new byte[0]));
        byte[] ef = carrier.recvFrame();
        boolean isErr = ef != null
            && Events.ERROR_NAME.equals(Events.Event.decode(ef, Events.Profile.VERBOSE).event());
        cases.check("unknown-method/failure", isErr, "expected $error");

        sendEvent(carrier, Events.Event.verbose(null, Events.CLOSE_NAME, new byte[0]));
        return cases;
    }

    // -----------------------------------------------------------------------
    // Datagrams (UDP over loopback)
    // -----------------------------------------------------------------------

    private static void datagramServer(DatagramSocket sock) {
        byte[] buf = new byte[65536];
        while (true) {
            DatagramPacket pkt = new DatagramPacket(buf, buf.length);
            try {
                sock.receive(pkt);
            } catch (IOException e) {
                continue;
            }
            byte[] data = Arrays.copyOf(pkt.getData(), pkt.getLength());
            Datagram dg;
            try {
                dg = Datagram.decode(data);
            } catch (RuntimeException e) {
                continue;
            }
            Datagram reply;
            if (dg.opOrd() == OP_ECHO_SCALARS) {
                try {
                    Scalars s = CsilCbor.decodeScalars(dg.payload());
                    reply = Datagram.of(OP_ECHO_SCALARS, dg.seq(), CsilCbor.encodeScalars(s));
                } catch (RuntimeException e) {
                    reply = Datagram.of(OP_ERROR_SENTINEL, dg.seq(), new byte[0]);
                }
            } else if (dg.opOrd() == OP_ECHO_COLLECTIONS) {
                try {
                    Collections c = CsilCbor.decodeCollections(dg.payload());
                    reply = Datagram.of(OP_ECHO_COLLECTIONS, dg.seq(), CsilCbor.encodeCollections(c));
                } catch (RuntimeException e) {
                    reply = Datagram.of(OP_ERROR_SENTINEL, dg.seq(), new byte[0]);
                }
            } else {
                reply = Datagram.of(OP_ERROR_SENTINEL, dg.seq(), new byte[0]);
            }
            // Reply to the datagram's source address/port.
            byte[] out = reply.encode();
            try {
                sock.send(new DatagramPacket(out, out.length, pkt.getAddress(), pkt.getPort()));
            } catch (IOException e) {
                // Drop on send failure; the client's read timeout will surface it.
            }
        }
    }

    private static Datagram roundtrip(DatagramSocket sock, long op, long seq, byte[] payload) {
        byte[] out = Datagram.of(op, seq, payload).encode();
        try {
            // The socket is connected to the server; target that peer explicitly.
            sock.send(new DatagramPacket(out, out.length, sock.getInetAddress(), sock.getPort()));
            byte[] buf = new byte[65536];
            DatagramPacket pkt = new DatagramPacket(buf, buf.length);
            sock.receive(pkt);
            return Datagram.decode(Arrays.copyOf(pkt.getData(), pkt.getLength()));
        } catch (IOException e) {
            throw new RuntimeException(e.toString());
        }
    }

    private static Cases datagramClient(DatagramSocket sock) {
        Cases cases = new Cases("datagrams");
        try {
            Datagram d = roundtrip(sock, OP_ECHO_SCALARS, 1, CsilCbor.encodeScalars(scalarsOk()));
            if (d.opOrd() == OP_ECHO_SCALARS) {
                Scalars s = CsilCbor.decodeScalars(d.payload());
                cases.check("echo-scalars/success", s.equals(scalarsOk()), "payload mismatch");
            } else {
                cases.fail("echo-scalars/success", "op_ord " + d.opOrd());
            }
        } catch (RuntimeException e) {
            cases.fail("echo-scalars/success", e.toString());
        }
        try {
            Datagram d = roundtrip(
                sock, OP_ECHO_COLLECTIONS, 2, CsilCbor.encodeCollections(collectionsOk()));
            if (d.opOrd() == OP_ECHO_COLLECTIONS) {
                Collections c = CsilCbor.decodeCollections(d.payload());
                cases.check("echo-collections/success", c.equals(collectionsOk()),
                    "payload mismatch");
            } else {
                cases.fail("echo-collections/success", "op_ord " + d.opOrd());
            }
        } catch (RuntimeException e) {
            cases.fail("echo-collections/success", e.toString());
        }
        try {
            Datagram d = roundtrip(sock, 99, 3, new byte[0]);
            cases.check("bad-op-ord/failure", d.opOrd() == OP_ERROR_SENTINEL,
                "expected error sentinel, got op_ord " + d.opOrd());
        } catch (RuntimeException e) {
            cases.fail("bad-op-ord/failure", e.toString());
        }
        return cases;
    }

    // -----------------------------------------------------------------------
    // Channel helpers + main
    // -----------------------------------------------------------------------

    private static FrameCarrier channelCarrier(SocketChannel conn) {
        InputStream in = Channels.newInputStream(conn);
        OutputStream out = Channels.newOutputStream(conn);
        return new Carriers.StreamCarrier(in, out);
    }

    // Bind the TCP listener with SO_REUSEADDR, retrying briefly so a just-freed port from
    // the previous server in the matrix is tolerated without hitting TCP TIME_WAIT.
    private static ServerSocketChannel bindRetry(int port) throws IOException {
        IOException last = null;
        for (int i = 0; i < 200; i++) {
            ServerSocketChannel ssc = ServerSocketChannel.open();
            try {
                ssc.setOption(StandardSocketOptions.SO_REUSEADDR, true);
                ssc.bind(new InetSocketAddress(InetAddress.getLoopbackAddress(), port));
                return ssc;
            } catch (IOException e) {
                last = e;
                ssc.close();
                try {
                    Thread.sleep(15);
                } catch (InterruptedException ignored) {
                    Thread.currentThread().interrupt();
                }
            }
        }
        throw last != null ? last : new IOException("bind failed");
    }

    private static SocketChannel connectRetry(int port) throws IOException {
        InetSocketAddress addr = new InetSocketAddress(InetAddress.getLoopbackAddress(), port);
        IOException last = null;
        for (int i = 0; i < 200; i++) {
            try {
                return SocketChannel.open(addr);
            } catch (IOException e) {
                last = e;
                try {
                    Thread.sleep(15);
                } catch (InterruptedException ignored) {
                    Thread.currentThread().interrupt();
                }
            }
        }
        throw last != null ? last : new IOException("connect failed");
    }

    public static void main(String[] args) throws Exception {
        if (args.length < 3) {
            System.err.println("usage: harness <server|client> <rpc|events|datagrams> <port>");
            System.exit(2);
        }
        String mode = args[0];
        String transport = args[1];
        int port = Integer.parseInt(args[2]);

        if (mode.equals("server") && transport.equals("datagrams")) {
            DatagramSocket sock = new DatagramSocket(null);
            sock.setReuseAddress(true);
            sock.bind(new InetSocketAddress(InetAddress.getLoopbackAddress(), port));
            System.out.println("READY");
            System.out.flush();
            datagramServer(sock);
        } else if (mode.equals("server")) {
            ServerSocketChannel listener = bindRetry(port);
            System.out.println("READY");
            System.out.flush();
            if (transport.equals("rpc")) {
                rpcServer(listener);
            } else if (transport.equals("events")) {
                eventsServer(listener);
            } else {
                System.err.println("bad transport " + transport);
                System.exit(2);
            }
        } else if (mode.equals("client") && transport.equals("datagrams")) {
            // Bind an ephemeral local UDP port; the server replies to this source.
            DatagramSocket sock = new DatagramSocket();
            sock.connect(new InetSocketAddress(InetAddress.getLoopbackAddress(), port));
            sock.setSoTimeout(3000);
            Cases cases = datagramClient(sock);
            cases.emit();
            sock.close();
        } else if (mode.equals("client")) {
            SocketChannel conn = connectRetry(port);
            Cases cases;
            if (transport.equals("rpc")) {
                cases = rpcClient(conn);
            } else if (transport.equals("events")) {
                cases = eventsClient(conn);
            } else {
                System.err.println("bad transport " + transport);
                System.exit(2);
                return;
            }
            cases.emit();
            conn.close();
        } else {
            System.err.println("bad mode/transport");
            System.exit(2);
        }
    }
}
