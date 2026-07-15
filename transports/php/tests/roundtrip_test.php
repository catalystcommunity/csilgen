<?php

require_once __DIR__ . '/bootstrap.php';

use Csilgen\Transport\CBOR;
use Csilgen\Transport\Conventions;
use Csilgen\Transport\Datagrams;
use Csilgen\Transport\Events;
use Csilgen\Transport\InMemoryCarrier;
use Csilgen\Transport\Rpc;

$value = array('name' => 'Ada', 'count' => 3, 'flags' => array(true, false, null));
assert_true(CBOR::decode(CBOR::encode($value)) == $value, 'cbor value roundtrip');

$encoded = CBOR::encode(array('b' => 2, 'a' => 1));
assert_true(bin2hex($encoded) === 'a2616101616202', 'canonical map ordering');

// -- CSIL-RPC request: all fields.
$payload = CBOR::encode($value);
$req = Rpc::decodeRequest(Rpc::encodeRequest('Attestation', 'deposit-claim', $payload, 7, 'bearer-tok'));
assert_true($req['service'] === 'Attestation', 'rpc request service roundtrip');
assert_true($req['op'] === 'deposit-claim', 'rpc request op roundtrip');
assert_true($req['id'] === 7, 'rpc request id roundtrip');
assert_true($req['auth'] === 'bearer-tok', 'rpc request auth roundtrip');
assert_true($req['payload'] === $payload, 'rpc request payload roundtrip');

// -- CSIL-RPC request: optional id/auth omitted on the wire and decoded as null.
$req = Rpc::decodeRequest(Rpc::encodeRequest('Attestation', 'deposit-claim', $payload));
assert_true($req['id'] === null, 'rpc request optional id absent');
assert_true($req['auth'] === null, 'rpc request optional auth absent');

// -- CSIL-RPC success response: status 0, variant selects the output arm.
$resp = Rpc::decodeResponse(Rpc::encodeResponse(Conventions::STATUS_OK, $payload, 7, 'DepositClaimResponse'));
assert_true($resp['status'] === 0, 'rpc response status roundtrip');
assert_true($resp['id'] === 7, 'rpc response id roundtrip');
assert_true($resp['variant'] === 'DepositClaimResponse', 'rpc response variant roundtrip');
assert_true($resp['error'] === null, 'rpc success response has no error');
assert_true($resp['payload'] === $payload, 'rpc response payload roundtrip');
assert_true(Rpc::checkStatus($resp) === $resp, 'checkStatus passes an ok response through');

// -- CSIL-RPC transport error: non-zero status, error string, empty payload.
$err = Rpc::decodeResponse(Rpc::encodeResponse(Conventions::STATUS_UNKNOWN_SERVICE_OR_OP, '', 7, null, 'no such op'));
assert_true($err['status'] === 2, 'rpc error response status roundtrip');
assert_true($err['variant'] === null, 'rpc error response has no variant');
assert_true($err['error'] === 'no such op', 'rpc error response error roundtrip');
assert_true($err['payload'] === '', 'rpc error response payload empty');
$thrown = assert_throws('Csilgen\Transport\StatusException', function () use ($err) {
    Rpc::checkStatus($err);
}, 'checkStatus raises on non-zero status');
assert_true($thrown->getStatusCode() === 2, 'StatusException carries the registry code');
assert_true($thrown->getStatusName() === 'unknown-service-or-op', 'StatusException carries the registry name');

// -- CSIL-RPC push.
$push = Rpc::decodePush(Rpc::encodePush('World', 'room-delta', $payload));
assert_true($push['service'] === 'World', 'rpc push service roundtrip');
assert_true($push['event'] === 'room-delta', 'rpc push event roundtrip');
assert_true($push['payload'] === $payload, 'rpc push payload roundtrip');

// -- Version mismatch: an unknown `v` is rejected, not silently misparsed.
$badVersion = CBOR::encode(array(
    'v' => 2,
    'service' => 'Attestation',
    'op' => 'deposit-claim',
    'payload' => Conventions::tag24($payload),
));
assert_throws('Csilgen\Transport\VersionException', function () use ($badVersion) {
    Rpc::decodeRequest($badVersion);
}, 'unknown transport version rejected');

$event = Events::decodeEvent(Events::encodeEvent(2, 'svc/op', CBOR::encode($value)));
assert_true($event['stream'] === 2, 'event stream roundtrip');

$dg = Datagrams::decode(Datagrams::encode('svc/op', CBOR::encode($value)));
assert_true($dg['method'] === 'svc/op', 'datagram method roundtrip');

$carrier = new InMemoryCarrier();
$carrier->send('abc');
assert_true($carrier->receive() === 'abc', 'carrier roundtrip');
