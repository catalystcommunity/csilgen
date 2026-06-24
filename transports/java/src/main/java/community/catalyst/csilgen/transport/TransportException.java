// Transport/protocol faults — malformed envelopes, CBOR errors, bad versions, frame
// overflow — modeled as an unchecked hierarchy: these are "the data/program is wrong"
// conditions, idiomatically unchecked in Java, and keep the codec API clean. Carrier
// I/O failures are a separate, checked concern (see Carrier).
package community.catalyst.csilgen.transport;

/** Base of the transport fault hierarchy. */
public class TransportException extends RuntimeException {
    public TransportException(String message) {
        super(message);
    }

    public TransportException(String message, Throwable cause) {
        super(message, cause);
    }
}
