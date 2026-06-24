package community.catalyst.csilgen.transport;

/** A carrier-level failure (a wrapped I/O error or a carrier protocol violation). */
public final class CarrierException extends TransportException {
    public CarrierException(String message) {
        super("carrier error: " + message);
    }

    public CarrierException(String message, Throwable cause) {
        super("carrier error: " + message, cause);
    }
}
