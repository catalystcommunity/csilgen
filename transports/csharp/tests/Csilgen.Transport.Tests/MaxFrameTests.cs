// The configurable max-frame guard (conventions doc section 5): a host sets the limit up or
// down through the carrier's public API, the limit applies to reads and writes alike, an
// oversized inbound length is rejected before allocation, and an invalid limit fails at
// construction rather than on the first frame.

using System;
using System.IO;
using Csilgen.Transport;
using Xunit;

namespace Csilgen.Transport.Tests;

/// <summary>Counts bytes handed out, so a test can prove the guard fires before the body is read.</summary>
internal sealed class CountingStream : Stream
{
    private readonly MemoryStream _inner;

    public CountingStream(byte[] bytes)
    {
        _inner = new MemoryStream(bytes);
    }

    public int ReadCount { get; private set; }

    public override bool CanRead => true;
    public override bool CanSeek => false;
    public override bool CanWrite => true;
    public override long Length => _inner.Length;

    public override long Position
    {
        get => _inner.Position;
        set => throw new NotSupportedException();
    }

    public override int Read(byte[] buffer, int offset, int count)
    {
        int n = _inner.Read(buffer, offset, count);
        ReadCount += n;
        return n;
    }

    public override void Write(byte[] buffer, int offset, int count) { }

    public override void Flush() { }

    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();

    public override void SetLength(long value) => throw new NotSupportedException();
}

public sealed class MaxFrameTests
{
    [Fact]
    public void DefaultLimitAcceptsFrameBelowIt()
    {
        using var stream = new MemoryStream();
        var frame = new byte[1024];
        Array.Fill(frame, (byte)0xAB);
        new StreamCarrier(stream).SendFrame(frame);

        stream.Position = 0;
        byte[]? got = new StreamCarrier(stream).RecvFrame();
        Assert.Equal(frame, got);
    }

    [Fact]
    public void DefaultLimitRejectsFrameAboveIt()
    {
        using var stream = new MemoryStream();
        var carrier = new StreamCarrier(stream);
        var frame = new byte[Conventions.MaxFrameDefault + 1];
        Assert.Throws<FrameTooLargeException>(() => carrier.SendFrame(frame));
        Assert.Equal(0, stream.Length);
    }

    [Fact]
    public void LargerCustomLimitAcceptsWhatDefaultRejects()
    {
        using var stream = new MemoryStream();
        int raised = Conventions.MaxFrameDefault + 4096;
        var frame = new byte[Conventions.MaxFrameDefault + 1];
        new StreamCarrier(stream, raised).SendFrame(frame);

        stream.Position = 0;
        byte[]? got = new StreamCarrier(stream, raised).RecvFrame();
        Assert.Equal(frame.Length, got!.Length);
    }

    [Fact]
    public void SmallerCustomLimitRejectsWhatDefaultAccepts()
    {
        using var stream = new MemoryStream();
        var carrier = new StreamCarrier(stream, 64);
        Assert.Throws<FrameTooLargeException>(() => carrier.SendFrame(new byte[1024]));
    }

    [Fact]
    public void OversizedIncomingLengthRejectedBeforeAllocation()
    {
        // A prefix claiming ~4 GiB followed by no body: if the guard ran after the read this
        // would allocate; it must fail on the prefix alone.
        using var stream = new CountingStream(new byte[] { 0xFF, 0xFF, 0xFF, 0xFF });
        var carrier = new StreamCarrier(stream, 4096);
        Assert.Throws<FrameTooLargeException>(() => carrier.RecvFrame());
        Assert.Equal(4, stream.ReadCount);
    }

    [Fact]
    public void InvalidLimitsRejectedAtConstruction()
    {
        // MaxFrameLimit is int.MaxValue, so no int argument can exceed it; the reachable
        // invalid values are all at or below zero.
        foreach (int limit in new[] { 0, -1, -4096, int.MinValue })
        {
            using var stream = new MemoryStream();
            Assert.Throws<InvalidMaxFrameException>(() => new StreamCarrier(stream, limit));
        }
    }

    [Fact]
    public void BoundaryLimitsAccepted()
    {
        foreach (int limit in new[] { 1, Conventions.MaxFrameDefault, Conventions.MaxFrameLimit })
        {
            using var stream = new MemoryStream();
            Assert.Equal(limit, new StreamCarrier(stream, limit).MaxFrame);
        }
    }
}
