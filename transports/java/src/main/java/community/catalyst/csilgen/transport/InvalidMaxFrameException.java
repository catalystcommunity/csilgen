package community.catalyst.csilgen.transport;

/** A host configured a max-frame limit outside the valid range. */
public final class InvalidMaxFrameException extends TransportException {
    public final long got;
    public final long limit;

    public InvalidMaxFrameException(long got, long limit) {
        super("max-frame limit of " + got + " is outside the valid range 1..=" + limit);
        this.got = got;
        this.limit = limit;
    }
}
