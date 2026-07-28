// Carrier seams -- the bring-your-own-carrier boundary (conventions doc §7).
//
// The library owns envelope codecs, framing, and lifecycle; the *carrier* (the byte/datagram
// transport) is injected. A host supplies QUIC, WebRTC, a platform media stack, or anything else by
// implementing one of these interfaces -- without changing the library. Everything here is
// synchronous and blocking: there is no async path anywhere in this library, by house rule.

using System.Buffers.Binary;
using System.Net.Sockets;

namespace Csilgen.Transport;

// IFrameCarrier sends and receives one *delimited message* at a time. Used by CSIL-RPC and
// CSIL-Events. Built-in implementations frame with a 4-byte big-endian length prefix; a host may
// implement this over WebSocket binary frames, a WebTransport stream, etc. IDisposable (synchronous
// Dispose, never IAsyncDisposable) gives callers `using`.
public interface IFrameCarrier : IDisposable
{
    void SendFrame(ReadOnlySpan<byte> frame);

    // RecvFrame receives the next frame, or null at a clean end of stream.
    byte[]? RecvFrame();
}

// IDatagramCarrier sends and receives one self-contained datagram (each within the channel MTU),
// with no delivery or ordering guarantee. Used by CSIL-Datagrams. Built-in over UDP; a host plugs
// WebRTC unreliable channels, QUIC datagrams, etc.
public interface IDatagramCarrier : IDisposable
{
    void SendDatagram(ReadOnlySpan<byte> datagram);

    // RecvDatagram receives the next datagram, or null if the carrier is closed.
    byte[]? RecvDatagram();
}

public static class Framing
{
    // WriteLengthPrefixed writes a 4-byte big-endian length prefix followed by the frame (CSIL
    // stream framing), enforcing the max-frame guard before writing.
    public static void WriteLengthPrefixed(Stream stream, ReadOnlySpan<byte> frame, int maxFrame)
    {
        if (frame.Length > maxFrame)
        {
            throw new FrameTooLargeException(frame.Length, maxFrame);
        }

        Span<byte> prefix = stackalloc byte[4];
        BinaryPrimitives.WriteUInt32BigEndian(prefix, (uint)frame.Length);
        try
        {
            stream.Write(prefix);
            stream.Write(frame);
        }
        catch (IOException ex)
        {
            throw new CarrierException(ex.Message);
        }
    }

    // ReadLengthPrefixed reads one length-prefixed frame, enforcing the max-frame guard before
    // allocating. It returns null at a clean end of stream before any byte of a frame.
    public static byte[]? ReadLengthPrefixed(Stream stream, int maxFrame)
    {
        Span<byte> lenBuf = stackalloc byte[4];
        int first;
        try
        {
            first = stream.Read(lenBuf[..1]);
        }
        catch (IOException ex)
        {
            throw new CarrierException(ex.Message);
        }

        // Zero bytes at the very start of a frame is an orderly end of stream.
        if (first == 0)
        {
            return null;
        }

        try
        {
            stream.ReadExactly(lenBuf[1..]);
        }
        catch (EndOfStreamException ex)
        {
            throw new CarrierException(ex.Message);
        }
        catch (IOException ex)
        {
            throw new CarrierException(ex.Message);
        }

        // Compare the prefix as an unsigned value before narrowing to int: a length >= 0x80000000
        // would become a negative int that slips past a signed > max guard and then throws when
        // allocating the buffer.
        uint length = BinaryPrimitives.ReadUInt32BigEndian(lenBuf);
        if (length > (uint)maxFrame)
        {
            throw new FrameTooLargeException((int)length, maxFrame);
        }

        byte[] buf = new byte[length];
        try
        {
            stream.ReadExactly(buf);
        }
        catch (EndOfStreamException ex)
        {
            throw new CarrierException(ex.Message);
        }
        catch (IOException ex)
        {
            throw new CarrierException(ex.Message);
        }

        return buf;
    }
}

// StreamCarrier is an IFrameCarrier over any duplex Stream (TCP NetworkStream, TLS, named pipe),
// using the canonical 4-byte length-prefix framing.
public sealed class StreamCarrier : IFrameCarrier
{
    private readonly Stream _stream;
    private readonly int _maxFrame;

    /// <summary>
    /// Builds a carrier with a host-chosen max-frame limit. The limit is validated here rather
    /// than at the first frame, so a misconfigured carrier is a construction-time error the host
    /// can surface at startup.
    /// </summary>
    public StreamCarrier(Stream stream, int maxFrame = Conventions.MaxFrameDefault)
    {
        Conventions.ValidateMaxFrame(maxFrame);
        _stream = stream;
        _maxFrame = maxFrame;
    }

    public void SendFrame(ReadOnlySpan<byte> frame) =>
        Framing.WriteLengthPrefixed(_stream, frame, _maxFrame);

    public byte[]? RecvFrame() => Framing.ReadLengthPrefixed(_stream, _maxFrame);

    /// <summary>The limit this carrier enforces in both directions.</summary>
    public int MaxFrame => _maxFrame;

    public void Dispose() => _stream.Dispose();
}

// LoopbackFrameCarrier is an in-memory IFrameCarrier backed by queues of frames -- for tests and for
// driving the codec without a socket.
public sealed class LoopbackFrameCarrier : IFrameCarrier
{
    private readonly Queue<byte[]> _outbound = new();
    private readonly Queue<byte[]> _inbound = new();

    // PushInbound queues a frame that a subsequent RecvFrame will return.
    public void PushInbound(byte[] frame) => _inbound.Enqueue((byte[])frame.Clone());

    // TakeOutbound takes the next frame that was sent via SendFrame, or null if none.
    public byte[]? TakeOutbound() => _outbound.Count == 0 ? null : _outbound.Dequeue();

    public void SendFrame(ReadOnlySpan<byte> frame) => _outbound.Enqueue(frame.ToArray());

    public byte[]? RecvFrame() => _inbound.Count == 0 ? null : _inbound.Dequeue();

    public void Dispose()
    {
    }
}

// LoopbackDatagramCarrier is an in-memory IDatagramCarrier -- for tests and codec drives.
public sealed class LoopbackDatagramCarrier : IDatagramCarrier
{
    private readonly Queue<byte[]> _outbound = new();
    private readonly Queue<byte[]> _inbound = new();

    public void PushInbound(byte[] datagram) => _inbound.Enqueue((byte[])datagram.Clone());

    public byte[]? TakeOutbound() => _outbound.Count == 0 ? null : _outbound.Dequeue();

    public void SendDatagram(ReadOnlySpan<byte> datagram) => _outbound.Enqueue(datagram.ToArray());

    public byte[]? RecvDatagram() => _inbound.Count == 0 ? null : _inbound.Dequeue();

    public void Dispose()
    {
    }
}

// UdpDatagramCarrier is an IDatagramCarrier over a connected UDP Socket. One datagram maps to one
// UDP packet; ordering and delivery are not guaranteed. The browser path (WebRTC unreliable /
// WebTransport) implements the same interface in the host.
public sealed class UdpDatagramCarrier : IDatagramCarrier
{
    private readonly Socket _socket;
    private readonly byte[] _recvBuf;

    // The receive buffer is one byte larger than the max so an oversized datagram fills past the max
    // and can be detected rather than silently truncated.
    public UdpDatagramCarrier(Socket socket)
    {
        _socket = socket;
        _recvBuf = new byte[Datagrams.MaxDatagramDefault + 1];
    }

    public void SendDatagram(ReadOnlySpan<byte> datagram)
    {
        try
        {
            _socket.Send(datagram);
        }
        catch (SocketException ex)
        {
            throw new CarrierException(ex.Message);
        }
    }

    public byte[]? RecvDatagram()
    {
        int n;
        try
        {
            n = _socket.Receive(_recvBuf);
        }
        catch (SocketException ex)
        {
            throw new CarrierException(ex.Message);
        }

        // Reading more than the max means the kernel truncated an oversized datagram, so the bytes
        // are incomplete and must not be handed to the decoder.
        if (n > Datagrams.MaxDatagramDefault)
        {
            throw new CarrierException("datagram exceeds max size");
        }

        return _recvBuf.AsSpan(0, n).ToArray();
    }

    public void Dispose() => _socket.Dispose();
}
