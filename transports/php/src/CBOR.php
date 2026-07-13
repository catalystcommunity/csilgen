<?php

namespace Csilgen\Transport;

final class Tag
{
    /** @var int */
    public $tag;
    /** @var mixed */
    public $value;

    public function __construct($tag, $value)
    {
        $this->tag = $tag;
        $this->value = $value;
    }
}

final class Bytes
{
    /** @var string */
    public $data;

    public function __construct($data)
    {
        $this->data = $data;
    }
}

final class CBOR
{
    public static function encode($value)
    {
        $out = '';
        self::encodeInto($out, $value);
        return $out;
    }

    private static function encodeInto(&$out, $value)
    {
        if ($value instanceof Bytes) {
            self::head($out, 2, strlen($value->data));
            $out .= $value->data;
        } elseif ($value instanceof Tag) {
            self::head($out, 6, $value->tag);
            self::encodeInto($out, $value->value);
        } elseif (is_int($value)) {
            if ($value < 0) {
                self::head($out, 1, -1 - $value);
            } else {
                self::head($out, 0, $value);
            }
        } elseif (is_string($value)) {
            self::head($out, 3, strlen($value));
            $out .= $value;
        } elseif (is_bool($value)) {
            $out .= chr($value ? 0xf5 : 0xf4);
        } elseif ($value === null) {
            $out .= chr(0xf6);
        } elseif (is_array($value)) {
            if (self::isList($value)) {
                self::head($out, 4, count($value));
                foreach ($value as $item) {
                    self::encodeInto($out, $item);
                }
            } else {
                $items = array();
                foreach ($value as $k => $v) {
                    $items[] = array(self::encode((string) $k), $v);
                }
                usort($items, function ($a, $b) {
                    return strcmp($a[0], $b[0]);
                });
                self::head($out, 5, count($items));
                foreach ($items as $item) {
                    $out .= $item[0];
                    self::encodeInto($out, $item[1]);
                }
            }
        } else {
            throw new CborException('cannot encode value of type ' . gettype($value));
        }
    }

    public static function bytes($bytes)
    {
        return new Bytes($bytes);
    }

    private static function head(&$out, $major, $arg)
    {
        $mt = $major << 5;
        if ($arg < 24) {
            $out .= chr($mt | $arg);
        } elseif ($arg < 0x100) {
            $out .= chr($mt | 24) . chr($arg);
        } elseif ($arg < 0x10000) {
            $out .= chr($mt | 25) . pack('n', $arg);
        } elseif ($arg < 0x100000000) {
            $out .= chr($mt | 26) . pack('N', $arg);
        } else {
            $hi = intdiv($arg, 0x100000000);
            $lo = $arg % 0x100000000;
            $out .= chr($mt | 27) . pack('NN', $hi, $lo);
        }
    }

    public static function decode($bytes)
    {
        $pos = 0;
        $value = self::decodeAt($bytes, $pos);
        if ($pos !== strlen($bytes)) {
            throw new CborException('trailing bytes after CBOR item');
        }
        return $value;
    }

    private static function decodeAt($bytes, &$pos)
    {
        self::need($bytes, $pos, 1);
        $initial = ord($bytes[$pos++]);
        $major = $initial >> 5;
        $info = $initial & 0x1f;

        if ($major === 7) {
            if ($info === 20) {
                return false;
            }
            if ($info === 21) {
                return true;
            }
            if ($info === 22) {
                return null;
            }
            throw new CborException('unsupported simple value');
        }

        $arg = self::argument($bytes, $pos, $info);
        if ($major === 0) {
            return $arg;
        }
        if ($major === 1) {
            return -1 - $arg;
        }
        if ($major === 2 || $major === 3) {
            self::need($bytes, $pos, $arg);
            $s = substr($bytes, $pos, $arg);
            $pos += $arg;
            return $s;
        }
        if ($major === 4) {
            $items = array();
            for ($i = 0; $i < $arg; $i++) {
                $items[] = self::decodeAt($bytes, $pos);
            }
            return $items;
        }
        if ($major === 5) {
            $map = array();
            for ($i = 0; $i < $arg; $i++) {
                $key = self::decodeAt($bytes, $pos);
                $map[$key] = self::decodeAt($bytes, $pos);
            }
            return $map;
        }
        if ($major === 6) {
            return new Tag($arg, self::decodeAt($bytes, $pos));
        }
        throw new CborException('unsupported CBOR major type ' . $major);
    }

    private static function argument($bytes, &$pos, $info)
    {
        if ($info < 24) {
            return $info;
        }
        if ($info === 24) {
            self::need($bytes, $pos, 1);
            return ord($bytes[$pos++]);
        }
        if ($info === 25) {
            self::need($bytes, $pos, 2);
            $v = unpack('n', substr($bytes, $pos, 2))[1];
            $pos += 2;
            return $v;
        }
        if ($info === 26) {
            self::need($bytes, $pos, 4);
            $v = unpack('N', substr($bytes, $pos, 4))[1];
            $pos += 4;
            return $v;
        }
        if ($info === 27) {
            self::need($bytes, $pos, 8);
            $p = unpack('Nhi/Nlo', substr($bytes, $pos, 8));
            $pos += 8;
            return $p['hi'] * 0x100000000 + $p['lo'];
        }
        throw new CborException('unsupported additional-info value ' . $info);
    }

    private static function need($bytes, $pos, $n)
    {
        if ($pos + $n > strlen($bytes)) {
            throw new CborException('unexpected end of CBOR input');
        }
    }

    private static function isList(array $value)
    {
        $i = 0;
        foreach ($value as $k => $_) {
            if ($k !== $i) {
                return false;
            }
            $i++;
        }
        return true;
    }
}
