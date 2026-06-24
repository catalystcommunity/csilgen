package community.catalyst.csilgen.transport;

/** Malformed or unsupported CBOR was encountered while decoding. */
public final class DecodeException extends TransportException {
    public DecodeException(String message) {
        super(message);
    }
}
