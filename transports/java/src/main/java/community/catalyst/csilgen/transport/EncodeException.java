package community.catalyst.csilgen.transport;

/** A value could not be encoded to canonical CBOR. */
public final class EncodeException extends TransportException {
    public EncodeException(String message) {
        super(message);
    }
}
