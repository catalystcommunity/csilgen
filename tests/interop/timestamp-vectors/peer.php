<?php
// Decode each vector as a `Stamp`; re-encode the rows that must decode.
// A PHP `DateTimeImmutable` holds microseconds, so this target holds 6 digits.
namespace Csilgen\Generated;

require getenv('CSILGEN_ROOT') . '/transports/php/src/Errors.php';
require getenv('CSILGEN_ROOT') . '/transports/php/src/CBOR.php';
require 'types.php';
require 'codec.php';

const DIGITS = 6;

$failures = 0;
foreach (file($argv[1], FILE_IGNORE_NEW_LINES) as $line) {
    if ($line === '' || $line[0] === '#') {
        continue;
    }
    list($name, $expect, $precision, $inputHex, $canonical) = explode("\t", $line);
    $mustDecode = $expect === 'ok' && (int)$precision <= DIGITS;
    try {
        $stamp = Codec::decodeStamp(hex2bin($inputHex));
        if (!($stamp->at instanceof \DateTimeImmutable)) {
            throw new \LogicException('decoded timestamp is not a DateTimeImmutable');
        }
        $encoded = bin2hex(Codec::encodeStamp($stamp));
    } catch (\RuntimeException $e) {
        if ($mustDecode) {
            $failures++;
            echo "FAIL $name: decode failed: {$e->getMessage()}\n";
        }
        continue;
    }
    if (!$mustDecode) {
        $failures++;
        echo "FAIL $name: decoded, but must fail (re-encoded $encoded)\n";
    } elseif ($encoded !== $canonical) {
        $failures++;
        echo "FAIL $name: re-encoded $encoded, want $canonical\n";
    }
}

if ($failures > 0) {
    echo "$failures vector(s) failed\n";
    exit(1);
}
echo "all timestamp vectors pass\n";
