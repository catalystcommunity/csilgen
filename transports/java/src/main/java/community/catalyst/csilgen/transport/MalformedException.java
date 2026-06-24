package community.catalyst.csilgen.transport;

/** A decoded envelope was structurally valid CBOR but not a well-formed envelope. */
public final class MalformedException extends TransportException {
    public MalformedException(String message) {
        super("malformed envelope: " + message);
    }
}
