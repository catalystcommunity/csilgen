<?php

// The configurable max-frame guard (conventions doc section 5): a host sets the
// limit up or down through the carrier's public API, the limit applies to reads and
// writes alike, an oversized inbound length is rejected before allocation, and an
// invalid limit fails at construction rather than on the first frame.

require_once __DIR__ . '/bootstrap.php';

use Csilgen\Transport\Conventions;
use Csilgen\Transport\FrameTooLargeException;
use Csilgen\Transport\InvalidMaxFrameException;
use Csilgen\Transport\StreamCarrier;

/** A rewindable in-memory stream standing in for a socket. */
function memory_stream($initial = '')
{
    $s = fopen('php://memory', 'r+b');
    if ($initial !== '') {
        fwrite($s, $initial);
        rewind($s);
    }
    return $s;
}

// -- 1. The default limit accepts a frame below it.
$stream = memory_stream();
$carrier = new StreamCarrier($stream);
$body = str_repeat("\xAB", 1024);
$carrier->send($body);
rewind($stream);
assert_true($carrier->receive() === $body, 'default limit round-trips a frame below it');

// -- 2. The default limit rejects a frame above it, without writing anything.
$stream = memory_stream();
$carrier = new StreamCarrier($stream);
assert_throws(
    FrameTooLargeException::class,
    function () use ($carrier) {
        $carrier->send(str_repeat("\x00", Conventions::MAX_FRAME_DEFAULT + 1));
    },
    'default limit rejects a frame above it'
);
assert_true(ftell($stream) === 0, 'a rejected frame must not put bytes on the wire');

// -- 3. A larger custom limit accepts what the default rejects.
$stream = memory_stream();
$raised = Conventions::MAX_FRAME_DEFAULT + 4096;
$carrier = new StreamCarrier($stream, $raised);
$big = str_repeat("\x00", Conventions::MAX_FRAME_DEFAULT + 1);
$carrier->send($big);
rewind($stream);
assert_true(strlen($carrier->receive()) === strlen($big), 'raised limit accepts and reads back');

// -- 4. A smaller custom limit rejects what the default accepts.
$carrier = new StreamCarrier(memory_stream(), 64);
assert_throws(
    FrameTooLargeException::class,
    function () use ($carrier) {
        $carrier->send(str_repeat("\xCD", 1024));
    },
    'lowered limit rejects a frame the default would accept'
);

// -- 5. An oversized incoming length is rejected before the body is allocated: the
// stream holds only the 4-byte prefix, so a guard that ran after the read would hang
// or over-allocate instead of failing here.
$stream = memory_stream("\xFF\xFF\xFF\xFF");
$carrier = new StreamCarrier($stream, 4096);
assert_throws(
    FrameTooLargeException::class,
    function () use ($carrier) {
        $carrier->receive();
    },
    'oversized inbound length rejected before allocation'
);
assert_true(ftell($stream) === 4, 'guard must fire on the 4-byte prefix alone');

// -- 6. Invalid limits are rejected at construction.
foreach (array(0, -1, -4096, Conventions::MAX_FRAME_LIMIT + 1, '4096', 1.5, null) as $limit) {
    assert_throws(
        InvalidMaxFrameException::class,
        function () use ($limit) {
            new StreamCarrier(memory_stream(), $limit);
        },
        'limit ' . var_export($limit, true) . ' must be rejected'
    );
}

// The boundary values are valid.
foreach (array(1, Conventions::MAX_FRAME_DEFAULT, Conventions::MAX_FRAME_LIMIT) as $limit) {
    $carrier = new StreamCarrier(memory_stream(), $limit);
    assert_true($carrier->getMaxFrame() === $limit, 'limit ' . $limit . ' must be accepted');
}

// -- A clean end of stream before any frame byte is an orderly close, not an error.
$carrier = new StreamCarrier(memory_stream(), 1024);
assert_true($carrier->receive() === null, 'clean EOF returns null');

// -- A zero-length frame round-trips: the prefix is 0x00000000 and the body empty.
$stream = memory_stream();
$carrier = new StreamCarrier($stream, 1024);
$carrier->send('');
rewind($stream);
assert_true($carrier->receive() === '', 'zero-length frame round-trips');
