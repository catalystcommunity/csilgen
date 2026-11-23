<?php
// Decode each vector as a `Moment`; re-encode the rows that must decode.
// A PHP `DateTimeImmutable` holds microseconds, so this target holds 6 digits.
namespace Csilgen\Generated;

require getenv('CSILGEN_ROOT') . '/transports/php/src/Errors.php';
require getenv('CSILGEN_ROOT') . '/transports/php/src/CBOR.php';
require 'types.php';
require 'codec.php';

const DIGITS = 6;

$checked = 0;
$failures = 0;
foreach (file($argv[1], FILE_IGNORE_NEW_LINES) as $line) {
    if ($line === '' || $line[0] === '#') {
        continue;
    }
    list($name, $expect, $digits, $inputHex, $canonical) = explode("\t", $line);
    if ($digits !== '*' && (int)$digits !== DIGITS) {
        continue;
    }
    $checked++;
    try {
        $moment = Codec::decodeMoment(hex2bin($inputHex));
        if (!($moment->at instanceof \DateTimeImmutable)) {
            throw new \LogicException('decoded cddl_time is not a DateTimeImmutable');
        }
        $encoded = bin2hex(Codec::encodeMoment($moment));
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
    echo "$failures of $checked epoch vector(s) failed\n";
    exit(1);
}
echo "all $checked epoch vectors pass\n";
