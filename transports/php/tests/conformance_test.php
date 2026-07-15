<?php

// Verify the PHP reference library against the checked-in conformance vectors.
// This both guards the encoders/decoders against drift and follows the contract
// every language's reference library implements: reconstruct each vector's
// envelope from its language-neutral `input`, assert encode -> `hex`, and assert
// decode(`hex`) -> the same envelope. Ported from transports/go/conformance_test.go.

require_once __DIR__ . '/bootstrap.php';

use Csilgen\Transport\Rpc;

$path = __DIR__ . '/../../conformance/rpc.json';
$doc = json_decode(file_get_contents($path), true);
assert_true(is_array($doc) && isset($doc['vectors']), 'conformance vectors load');

foreach ($doc['vectors'] as $vec) {
    $name = $vec['name'];
    $in = $vec['input'];
    $payload = hex2bin($in['payload_hex']);

    switch ($in['kind']) {
        case 'request':
            $encoded = Rpc::encodeRequest($in['service'], $in['op'], $payload, $in['id'], $in['auth']);
            $decoded = Rpc::decodeRequest(hex2bin($vec['hex']));
            $expected = array(
                'service' => $in['service'],
                'op' => $in['op'],
                'id' => $in['id'],
                'payload' => $payload,
                'auth' => $in['auth'],
            );
            break;
        case 'response':
            $encoded = Rpc::encodeResponse($in['status'], $payload, $in['id'], $in['variant'], $in['error']);
            $decoded = Rpc::decodeResponse(hex2bin($vec['hex']));
            $expected = array(
                'id' => $in['id'],
                'status' => $in['status'],
                'variant' => $in['variant'],
                'error' => $in['error'],
                'payload' => $payload,
            );
            break;
        case 'push':
            $encoded = Rpc::encodePush($in['service'], $in['event'], $payload);
            $decoded = Rpc::decodePush(hex2bin($vec['hex']));
            $expected = array(
                'service' => $in['service'],
                'event' => $in['event'],
                'payload' => $payload,
            );
            break;
        default:
            assert_true(false, 'unknown rpc vector kind ' . $in['kind']);
            return;
    }

    assert_true(bin2hex($encoded) === $vec['hex'], "encode $name matches vector bytes");
    assert_true($decoded === $expected, "decode $name matches vector input");
}
