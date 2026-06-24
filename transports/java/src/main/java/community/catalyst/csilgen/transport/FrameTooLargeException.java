package community.catalyst.csilgen.transport;

/** A frame exceeded the max-frame guard; rejected before allocating for it. */
public final class FrameTooLargeException extends TransportException {
    public final long got;
    public final long max;

    public FrameTooLargeException(long got, long max) {
        super("frame of " + got + " bytes exceeds max-frame guard of " + max + " bytes");
        this.got = got;
        this.max = max;
    }
}
