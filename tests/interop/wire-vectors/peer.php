<?php
// Decode each vector as the shape its kind names; re-encode the rows that must decode.
namespace Csilgen\Generated;

require getenv('CSILGEN_ROOT') . '/transports/php/src/Errors.php';
require getenv('CSILGEN_ROOT') . '/transports/php/src/CBOR.php';
require 'types.php';
require 'codec.php';

function roundTrip($kind, $bytes)
{
    switch ($kind) {
        case 'holder':
            return Codec::encodeHolder(Codec::decodeHolder($bytes));
        case 'contact':
            return Codec::encodeContact(Codec::decodeContact($bytes));
        case 'output':
            return Codec::encodeLookupReadCountsOutput(Codec::decodeLookupReadCountsOutput($bytes));
    }
    throw new \LogicException("unknown kind $kind");
}

$failures = 0;
foreach (file($argv[1], FILE_IGNORE_NEW_LINES) as $line) {
    if ($line === '' || $line[0] === '#') {
        continue;
    }
    list($name, $kind, $expect, $inputHex, $canonical) = explode("\t", $line);
    // Only the codec's own exceptions count as a rejection; a PHP error is a bug.
    try {
        $encoded = bin2hex(roundTrip($kind, hex2bin($inputHex)));
    } catch (\RuntimeException $e) {
        if ($expect === 'ok') {
            $failures++;
            echo "FAIL $name: decode failed: {$e->getMessage()}\n";
        }
        continue;
    }
    if ($expect !== 'ok') {
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
echo "all wire vectors pass\n";
