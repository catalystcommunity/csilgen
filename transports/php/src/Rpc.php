<?php

namespace Csilgen\Transport;

final class Rpc
{
    public static function encodeRequest($id, $method, $payload)
    {
        return CBOR::encode(array('id' => $id, 'method' => $method, 'payload' => new Tag(24, CBOR::bytes($payload))));
    }

    public static function decodeRequest($bytes)
    {
        $m = CBOR::decode($bytes);
        return array('id' => $m['id'], 'method' => $m['method'], 'payload' => $m['payload']->value);
    }

    public static function encodeResponse($id, $status, $payload)
    {
        return CBOR::encode(array('id' => $id, 'status' => $status, 'payload' => new Tag(24, CBOR::bytes($payload))));
    }

    public static function decodeResponse($bytes)
    {
        $m = CBOR::decode($bytes);
        return array('id' => $m['id'], 'status' => $m['status'], 'payload' => $m['payload']->value);
    }
}
