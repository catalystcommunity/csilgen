// A transport-level status. It is distinct from application errors, which ride inside
// the payload as a declared `/ ErrorType` arm (conventions doc §4). Equality is by the
// underlying code, so host-defined extension codes (>= 64) and unknown codes compare
// correctly.
package community.catalyst.csilgen.transport;

public record Status(long code) {
    public static final Status OK = new Status(0);
    public static final Status MALFORMED_ENVELOPE = new Status(1);
    public static final Status UNKNOWN_SERVICE_OR_OP = new Status(2);
    public static final Status UNAUTHENTICATED = new Status(3);
    public static final Status FORBIDDEN = new Status(4);
    public static final Status VERSION_UNSUPPORTED = new Status(5);
    public static final Status INTERNAL = new Status(6);
    public static final Status UNAVAILABLE = new Status(7);
    public static final Status DEADLINE_EXCEEDED = new Status(8);

    /** Maps a wire code onto a Status, preserving host-defined and unknown codes. */
    public static Status fromCode(long code) {
        return new Status(code);
    }

    /** Whether the status indicates a typed reply is present. */
    public boolean isOk() {
        return code == 0;
    }

    /** The registry name for the status, or "other" for codes outside the registry. */
    public String name() {
        return switch ((int) code) {
            case 0 -> "ok";
            case 1 -> "malformed-envelope";
            case 2 -> "unknown-service-or-op";
            case 3 -> "unauthenticated";
            case 4 -> "forbidden";
            case 5 -> "version-unsupported";
            case 6 -> "internal";
            case 7 -> "unavailable";
            case 8 -> "deadline-exceeded";
            default -> "other";
        };
    }
}
