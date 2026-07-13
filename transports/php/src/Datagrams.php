<?php

namespace Csilgen\Transport;

final class Datagrams
{
    public static function encode($method, $payload)
    {
        return CBOR::encode(array('method' => $method, 'payload' => new Tag(24, CBOR::bytes($payload))));
    }

    public static function decode($bytes)
    {
        $m = CBOR::decode($bytes);
        return array('method' => $m['method'], 'payload' => $m['payload']->value);
    }
}
