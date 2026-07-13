<?php

namespace Csilgen\Transport;

final class Events
{
    public static function encodeEvent($streamId, $method, $payload)
    {
        return CBOR::encode(array('stream' => $streamId, 'method' => $method, 'payload' => new Tag(24, CBOR::bytes($payload))));
    }

    public static function decodeEvent($bytes)
    {
        $m = CBOR::decode($bytes);
        return array('stream' => $m['stream'], 'method' => $m['method'], 'payload' => $m['payload']->value);
    }
}
