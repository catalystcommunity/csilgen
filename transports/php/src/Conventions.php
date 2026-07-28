<?php

namespace Csilgen\Transport;

final class Conventions
{
    /** Current transport version; minted anew only for breaking envelope changes. */
    const VERSION = 1;

    /** CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949 3.4.5.1). */
    const TAG_ENCODED_CBOR = 24;

    /** Default max encoded envelope size for stream/message carriers (16 MiB). A
     * carrier rejects anything larger before allocating for it. */
    const MAX_FRAME_DEFAULT = 16777216;

    /** Largest max-frame limit a host may configure (2 GiB - 1). The 4-byte length
     * prefix can express more, but the limit lives in a signed 32-bit integer in
     * several supported languages, so this is the portable ceiling every CSIL
     * transport agrees on. Anything above it would also disable the receive guard
     * outright, since no wire length could exceed it. */
    const MAX_FRAME_LIMIT = 2147483647;

    /** Check a host-supplied max-frame limit against the conventions doc range
     * (1..MAX_FRAME_LIMIT), so an unusable carrier fails at construction rather
     * than on its first frame. */
    public static function validateMaxFrame($maxFrame)
    {
        if (!is_int($maxFrame) || $maxFrame < 1 || $maxFrame > self::MAX_FRAME_LIMIT) {
            throw new InvalidMaxFrameException($maxFrame, self::MAX_FRAME_LIMIT);
        }
        return $maxFrame;
    }

    // Transport status registry -- see csil-transport-conventions.md section 4.
    const STATUS_OK = 0;
    const STATUS_MALFORMED_ENVELOPE = 1;
    const STATUS_UNKNOWN_SERVICE_OR_OP = 2;
    const STATUS_UNAUTHENTICATED = 3;
    const STATUS_FORBIDDEN = 4;
    const STATUS_VERSION_UNSUPPORTED = 5;
    const STATUS_INTERNAL = 6;
    const STATUS_UNAVAILABLE = 7;
    const STATUS_DEADLINE_EXCEEDED = 8;

    public static function statusName($code)
    {
        switch ($code) {
            case self::STATUS_OK:
                return 'ok';
            case self::STATUS_MALFORMED_ENVELOPE:
                return 'malformed-envelope';
            case self::STATUS_UNKNOWN_SERVICE_OR_OP:
                return 'unknown-service-or-op';
            case self::STATUS_UNAUTHENTICATED:
                return 'unauthenticated';
            case self::STATUS_FORBIDDEN:
                return 'forbidden';
            case self::STATUS_VERSION_UNSUPPORTED:
                return 'version-unsupported';
            case self::STATUS_INTERNAL:
                return 'internal';
            case self::STATUS_UNAVAILABLE:
                return 'unavailable';
            case self::STATUS_DEADLINE_EXCEEDED:
                return 'deadline-exceeded';
            default:
                return 'other';
        }
    }

    public static function checkVersion($v)
    {
        if ($v !== self::VERSION) {
            throw new VersionException('unsupported transport version ' . var_export($v, true));
        }
    }

    public static function tag24($payload)
    {
        return new Tag(self::TAG_ENCODED_CBOR, CBOR::bytes($payload));
    }

    public static function untag24($value)
    {
        // The decoder returns byte strings as plain PHP strings, so the inner
        // check is is_string rather than a Bytes wrapper.
        if (!($value instanceof Tag) || $value->tag !== self::TAG_ENCODED_CBOR || !is_string($value->value)) {
            throw new MalformedException('expected a tag-24 (encoded-cbor) payload');
        }
        return $value->value;
    }

    public static function decodeEnvelope($bytes)
    {
        $value = CBOR::decode($bytes);
        if (!is_array($value)) {
            throw new MalformedException('envelope is not a CBOR map');
        }
        return $value;
    }

    public static function getUint(array $map, $key)
    {
        if (!isset($map[$key]) || !is_int($map[$key]) || $map[$key] < 0) {
            throw new MalformedException("missing or non-integer field '" . $key . "'");
        }
        return $map[$key];
    }

    public static function getInt(array $map, $key)
    {
        if (!isset($map[$key]) || !is_int($map[$key])) {
            throw new MalformedException("missing or non-integer field '" . $key . "'");
        }
        return $map[$key];
    }

    public static function getText(array $map, $key)
    {
        if (!isset($map[$key]) || !is_string($map[$key])) {
            throw new MalformedException("missing or non-text field '" . $key . "'");
        }
        return $map[$key];
    }

    public static function getUintOpt(array $map, $key)
    {
        return isset($map[$key]) && is_int($map[$key]) && $map[$key] >= 0 ? $map[$key] : null;
    }

    public static function getTextOpt(array $map, $key)
    {
        return isset($map[$key]) && is_string($map[$key]) ? $map[$key] : null;
    }

    /**
     * Legacy collapsed "service/op" naming from the pre-v1 draft envelopes. The
     * CSIL-RPC v1 envelope carries `service` and `op` as separate verbatim fields,
     * so new code should not need this.
     */
    public static function methodName($service, $operation)
    {
        return self::kebab($service) . '/' . self::kebab($operation);
    }

    private static function kebab($name)
    {
        $name = preg_replace('/([a-z0-9])([A-Z])/', '$1-$2', $name);
        $name = preg_replace('/[^A-Za-z0-9]+/', '-', $name);
        return strtolower(trim($name, '-'));
    }
}
