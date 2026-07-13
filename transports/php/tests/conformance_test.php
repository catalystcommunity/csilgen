<?php

require_once __DIR__ . '/bootstrap.php';

use Csilgen\Transport\CBOR;
use Csilgen\Transport\Rpc;

$encoded = CBOR::encode(array('b' => 2, 'a' => 1));
assert_true(bin2hex($encoded) === 'a2616101616202', 'canonical map ordering');

$payload = CBOR::encode(array('ok' => true));
$req = Rpc::decodeRequest(Rpc::encodeRequest(7, 'task/create', $payload));
assert_true($req['id'] === 7, 'rpc id roundtrip');
assert_true($req['method'] === 'task/create', 'rpc method roundtrip');
assert_true($req['payload'] === $payload, 'rpc payload roundtrip');
