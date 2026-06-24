package community.catalyst.csilgen.transport;

/** An envelope carried a transport version this library does not support. */
public final class UnsupportedVersionException extends TransportException {
    public final long version;

    public UnsupportedVersionException(long version) {
        super("unsupported transport version " + version);
        this.version = version;
    }
}
