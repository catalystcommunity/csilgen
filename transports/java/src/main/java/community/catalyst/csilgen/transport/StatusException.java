package community.catalyst.csilgen.transport;

/** A non-zero transport status returned by a peer, distinct from application errors. */
public final class StatusException extends TransportException {
    public final Status status;
    public final String detail;

    public StatusException(Status status, String detail) {
        super(detail == null || detail.isEmpty()
                ? "transport status " + status.name() + " (" + status.code() + ")"
                : "transport status " + status.name() + " (" + status.code() + "): " + detail);
        this.status = status;
        this.detail = detail;
    }
}
