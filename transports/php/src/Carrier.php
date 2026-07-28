<?php

namespace Csilgen\Transport;

interface ByteCarrier
{
    public function send($bytes);
    public function receive();
}

final class InMemoryCarrier implements ByteCarrier
{
    /** @var array<int,string> */
    private $queue = array();

    public function send($bytes)
    {
        $this->queue[] = $bytes;
    }

    public function receive()
    {
        if (!$this->queue) {
            throw new CarrierException('empty in-memory carrier');
        }
        return array_shift($this->queue);
    }
}

/**
 * CSIL stream framing: a 4-byte big-endian length prefix in front of each encoded
 * envelope. PHP has no single stream abstraction that covers sockets, TLS wrappers
 * and in-memory buffers alike, so the framing is exposed as functions over a PHP
 * stream resource and the carrier below wraps a resource with them.
 */
final class Framing
{
    /**
     * Write a length-prefixed frame, enforcing the max-frame guard before writing
     * anything.
     *
     * @param resource $stream
     * @param string   $frame
     * @param int      $maxFrame
     */
    public static function writeLengthPrefixed($stream, $frame, $maxFrame)
    {
        Conventions::validateMaxFrame($maxFrame);
        $length = strlen($frame);
        if ($length > $maxFrame) {
            throw new FrameTooLargeException($length, $maxFrame);
        }
        // 'N' is a 32-bit big-endian unsigned long -- the CSIL prefix exactly.
        $written = @fwrite($stream, pack('N', $length) . $frame);
        if ($written === false) {
            throw new CarrierException('write failed');
        }
    }

    /**
     * Read one length-prefixed frame, enforcing the max-frame guard before
     * allocating for the body. Returns null at a clean end of stream before any
     * frame byte.
     *
     * @param resource $stream
     * @param int      $maxFrame
     * @return string|null
     */
    public static function readLengthPrefixed($stream, $maxFrame)
    {
        Conventions::validateMaxFrame($maxFrame);
        $prefix = self::readExactly($stream, 4, true);
        if ($prefix === null) {
            return null;
        }
        $unpacked = unpack('N', $prefix);
        // The prefix is read as an unsigned 32-bit value and compared before the
        // body is allocated, so a forged length can never drive a giant read. On a
        // 32-bit PHP build a length >= 0x80000000 arrives as a float, which still
        // compares correctly against the guard.
        $length = $unpacked[1];
        if ($length > $maxFrame) {
            throw new FrameTooLargeException($length, $maxFrame);
        }
        if ($length === 0) {
            return '';
        }
        $body = self::readExactly($stream, $length, false);
        if ($body === null) {
            throw new CarrierException('connection closed mid-frame');
        }
        return $body;
    }

    /**
     * Read exactly $n bytes. When $eofOk, a clean end of stream before the first
     * byte returns null (an orderly close between frames) rather than raising.
     *
     * @param resource $stream
     * @return string|null
     */
    private static function readExactly($stream, $n, $eofOk)
    {
        $buf = '';
        while (strlen($buf) < $n) {
            $chunk = @fread($stream, $n - strlen($buf));
            if ($chunk === false || $chunk === '') {
                if ($buf === '' && $eofOk) {
                    return null;
                }
                return null;
            }
            $buf .= $chunk;
        }
        return $buf;
    }
}

/**
 * A ByteCarrier over a PHP stream resource (a socket, a TLS-wrapped stream, a
 * php://memory buffer), using the canonical 4-byte length-prefix framing.
 */
final class StreamCarrier implements ByteCarrier
{
    /** @var resource */
    private $stream;
    /** @var int */
    private $maxFrame;

    /**
     * @param resource $stream
     * @param int      $maxFrame The limit is validated here rather than at the first
     *                           frame, so a misconfigured carrier is a
     *                           construction-time error the host surfaces at startup.
     */
    public function __construct($stream, $maxFrame = Conventions::MAX_FRAME_DEFAULT)
    {
        $this->stream = $stream;
        $this->maxFrame = Conventions::validateMaxFrame($maxFrame);
    }

    public function send($bytes)
    {
        Framing::writeLengthPrefixed($this->stream, $bytes, $this->maxFrame);
    }

    public function receive()
    {
        return Framing::readLengthPrefixed($this->stream, $this->maxFrame);
    }

    /** The limit this carrier enforces in both directions. */
    public function getMaxFrame()
    {
        return $this->maxFrame;
    }
}
