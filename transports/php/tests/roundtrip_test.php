<?php

require_once __DIR__ . '/bootstrap.php';

use Csilgen\Transport\CBOR;
use Csilgen\Transport\Datagrams;
use Csilgen\Transport\Events;
use Csilgen\Transport\InMemoryCarrier;

$value = array('name' => 'Ada', 'count' => 3, 'flags' => array(true, false, null));
assert_true(CBOR::decode(CBOR::encode($value)) == $value, 'cbor value roundtrip');

$event = Events::decodeEvent(Events::encodeEvent(2, 'svc/op', CBOR::encode($value)));
assert_true($event['stream'] === 2, 'event stream roundtrip');

$dg = Datagrams::decode(Datagrams::encode('svc/op', CBOR::encode($value)));
assert_true($dg['method'] === 'svc/op', 'datagram method roundtrip');

$carrier = new InMemoryCarrier();
$carrier->send('abc');
assert_true($carrier->receive() === 'abc', 'carrier roundtrip');
