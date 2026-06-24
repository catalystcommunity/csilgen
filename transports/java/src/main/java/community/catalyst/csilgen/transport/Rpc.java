// CSIL-RPC transport — request/response/push envelopes — see csil-rpc-transport.md.
// Synchronous and blocking: the client owns the envelope and a per-connection monotonic
// correlation id; the carrier is injected. No CompletableFuture, ever.
package community.catalyst.csilgen.transport;

import java.io.IOException;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Objects;

import static community.catalyst.csilgen.transport.Conventions.*;

public final class Rpc {
    private Rpc() {}

    /**
     * A CSIL-RPC request (client → server). A null {@code id}/{@code auth} means the field
     * is absent on the wire (not a present null).
     */
    public record Request(String service, String op, Long id, byte[] payload, String auth) {
        /** A request with no correlation id and no per-request auth. */
        public static Request of(String service, String op, byte[] payload) {
            return new Request(service, op, null, payload, null);
        }

        public Request withId(long id) {
            return new Request(service, op, id, payload, auth);
        }

        public Request withAuth(String auth) {
            return new Request(service, op, id, payload, auth);
        }

        public byte[] encode() {
            List<CEntry> entries = new ArrayList<>();
            entries.add(entry("v", new CUint(VERSION)));
            entries.add(textEntry("service", service));
            entries.add(textEntry("op", op));
            entries.add(entry("payload", tag24(payload)));
            if (id != null) {
                entries.add(entry("id", new CUint(id)));
            }
            if (auth != null) {
                entries.add(textEntry("auth", auth));
            }
            return encodeMap(entries);
        }

        public static Request decode(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            checkVersion(getUint(v, "v"));
            CborValue p = mapGet(v, "payload");
            if (p == null) {
                throw new MalformedException("missing 'payload'");
            }
            return new Request(
                    getText(v, "service"),
                    getText(v, "op"),
                    getUintOpt(v, "id"),
                    untag24(p),
                    getTextOpt(v, "auth"));
        }

        // byte[] payload needs content equality so a decoded request equals its original.
        @Override
        public boolean equals(Object o) {
            return o instanceof Request r
                    && Objects.equals(service, r.service)
                    && Objects.equals(op, r.op)
                    && Objects.equals(id, r.id)
                    && Arrays.equals(payload, r.payload)
                    && Objects.equals(auth, r.auth);
        }

        @Override
        public int hashCode() {
            return Objects.hash(service, op, id, Arrays.hashCode(payload), auth);
        }
    }

    /**
     * A CSIL-RPC response (server → client). {@code variant} names which declared
     * output-choice arm {@code payload} decodes to; {@code payload} is a present but empty
     * tag-24 byte string when {@code status} is non-zero.
     */
    public record Response(Long id, Status status, String variant, String error, byte[] payload) {
        /** A successful (status ok) typed reply. */
        public static Response ok(String variant, byte[] payload) {
            return new Response(null, Status.OK, variant, null, payload);
        }

        /** A transport-level failure (no typed payload). */
        public static Response transportError(Status status, String message) {
            return new Response(null, status, null, message, new byte[0]);
        }

        public Response withId(Long id) {
            return new Response(id, status, variant, error, payload);
        }

        public byte[] encode() {
            List<CEntry> entries = new ArrayList<>();
            entries.add(entry("v", new CUint(VERSION)));
            entries.add(entry("status", new CInt(status.code())));
            entries.add(entry("payload", tag24(payload)));
            if (id != null) {
                entries.add(entry("id", new CUint(id)));
            }
            if (variant != null) {
                entries.add(textEntry("variant", variant));
            }
            if (error != null) {
                entries.add(textEntry("error", error));
            }
            return encodeMap(entries);
        }

        public static Response decode(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            checkVersion(getUint(v, "v"));
            // payload is present but may be an empty byte string on error.
            byte[] payload = new byte[0];
            CborValue p = mapGet(v, "payload");
            if (p != null) {
                payload = untag24(p);
            }
            return new Response(
                    getUintOpt(v, "id"),
                    Status.fromCode(getInt(v, "status")),
                    getTextOpt(v, "variant"),
                    getTextOpt(v, "error"),
                    payload);
        }

        /**
         * A StatusException for a non-ok response, or null when the response carries a
         * typed reply (status 0). Surfaces transport failures distinctly from app errors.
         */
        public StatusException asTransportError() {
            if (status.isOk()) {
                return null;
            }
            return new StatusException(status, error == null ? "" : error);
        }

        @Override
        public boolean equals(Object o) {
            return o instanceof Response r
                    && Objects.equals(id, r.id)
                    && Objects.equals(status, r.status)
                    && Objects.equals(variant, r.variant)
                    && Objects.equals(error, r.error)
                    && Arrays.equals(payload, r.payload);
        }

        @Override
        public int hashCode() {
            return Objects.hash(id, status, variant, error, Arrays.hashCode(payload));
        }
    }

    /** A CSIL-RPC server push (server → client) for {@code <-} operations. */
    public record Push(String service, String event, byte[] payload) {
        public static Push of(String service, String event, byte[] payload) {
            return new Push(service, event, payload);
        }

        public byte[] encode() {
            List<CEntry> entries = new ArrayList<>();
            entries.add(entry("v", new CUint(VERSION)));
            entries.add(textEntry("service", service));
            entries.add(textEntry("event", event));
            entries.add(entry("payload", tag24(payload)));
            return encodeMap(entries);
        }

        public static Push decode(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            checkVersion(getUint(v, "v"));
            CborValue p = mapGet(v, "payload");
            if (p == null) {
                throw new MalformedException("missing 'payload'");
            }
            return new Push(getText(v, "service"), getText(v, "event"), untag24(p));
        }

        @Override
        public boolean equals(Object o) {
            return o instanceof Push r
                    && Objects.equals(service, r.service)
                    && Objects.equals(event, r.event)
                    && Arrays.equals(payload, r.payload);
        }

        @Override
        public int hashCode() {
            return Objects.hash(service, event, Arrays.hashCode(payload));
        }
    }

    /**
     * What a server handler returns for one request: a typed reply (variant + payload) on
     * success, or a transport status on failure.
     */
    public record HandlerOutcome(boolean isReply, String variant, byte[] payload, Status status,
            String message) {
        public static HandlerOutcome reply(String variant, byte[] payload) {
            return new HandlerOutcome(true, variant, payload, Status.OK, "");
        }

        public static HandlerOutcome transport(Status status, String message) {
            return new HandlerOutcome(false, "", new byte[0], status, message);
        }
    }

    /** The host's request handler; the generated router is the natural implementation. */
    @FunctionalInterface
    public interface Handler {
        HandlerOutcome handle(Request request);
    }

    /**
     * A CSIL-RPC client over a frame carrier. The carrier is injected (bring your own); the
     * client owns the envelope and a per-connection monotonic correlation id.
     */
    public static final class Client {
        private final FrameCarrier carrier;
        private final boolean multiplexed;
        private long nextId = 1;

        /**
         * @param multiplexed true assigns a correlation id to every request (required on
         *     WS / pipelined streams); false omits it (one-in-flight carriers such as HTTP).
         */
        public Client(FrameCarrier carrier, boolean multiplexed) {
            this.carrier = carrier;
            this.multiplexed = multiplexed;
        }

        public FrameCarrier carrier() {
            return carrier;
        }

        /**
         * Invokes service/op with an encoded request payload, returning the decoded
         * response. A non-zero transport status is surfaced as a StatusException.
         */
        public Response call(String service, String op, byte[] payload, String auth)
                throws IOException {
            Request req = new Request(service, op, null, payload, auth);
            if (multiplexed) {
                req = req.withId(nextId);
                nextId++;
            }
            carrier.sendFrame(req.encode());
            byte[] in = carrier.recvFrame();
            if (in == null) {
                throw new CarrierException("connection closed before response");
            }
            Response resp = Response.decode(in);
            StatusException te = resp.asTransportError();
            if (te != null) {
                throw te;
            }
            return resp;
        }
    }

    /**
     * A CSIL-RPC server over a frame carrier. The host supplies a handler mapping a request
     * to an outcome; the generated router is the natural implementation of that handler.
     */
    public static final class Server {
        private final FrameCarrier carrier;

        public Server(FrameCarrier carrier) {
            this.carrier = carrier;
        }

        /**
         * Reads one request, dispatches it through handler, and writes the response. Returns
         * false at a clean end of stream.
         */
        public boolean serveOne(Handler handler) throws IOException {
            byte[] frame = carrier.recvFrame();
            if (frame == null) {
                return false;
            }
            Response resp;
            try {
                Request req = Request.decode(frame);
                HandlerOutcome outcome = handler.handle(req);
                if (outcome.isReply()) {
                    resp = Response.ok(outcome.variant(), outcome.payload()).withId(req.id());
                } else {
                    resp = Response.transportError(outcome.status(), outcome.message())
                            .withId(req.id());
                }
            } catch (TransportException e) {
                resp = Response.transportError(Status.MALFORMED_ENVELOPE, e.getMessage());
            }
            carrier.sendFrame(resp.encode());
            return true;
        }
    }
}
