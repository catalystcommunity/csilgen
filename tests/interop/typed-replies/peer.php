<?php
namespace Csilgen\Generated;

require getenv('CSILGEN_ROOT') . '/transports/php/src/CBOR.php';
require 'types.php';
require 'codec.php';
require 'server.php';
require 'client.php';

assert(RpErrorCode::toNumber(RpErrorCode::ACCOUNT_RATE_LIMITED) === 42);
assert(RpErrorCode::fromNumber(42) === RpErrorCode::ACCOUNT_RATE_LIMITED);

class Handler implements RpHandler
{
    public function decryptToken($req)
    {
        if ($req->a === 'limit') {
            throw new RpErrorRefusal(new RpError(['code' => RpErrorCode::APPLICATION_USER_LIMIT, 'message' => 'limit']));
        }
        return new Resp(['b' => 'ok']);
    }
}

class Carrier
{
    public function call($service, $op, $payload)
    {
        if (Codec::decodeReq($payload)->a === 'unknown') {
            throw new \RuntimeException('unknown operation', 2);
        }
        $reply = (new RpRouter(new Handler()))->dispatch($op, $payload);
        if ($reply['variant'] === 'RpError') assert(ord($reply['payload'][6]) === 7);
        return Codec::resolveRpcReply($service, $op, $reply['variant'], $reply['payload']);
    }
}

$client = new RpClient(new Carrier());
assert($client->decryptToken(new Req(['a' => 'ok']))->b === 'ok');
try {
    $client->decryptToken(new Req(['a' => 'limit']));
    throw new \LogicException('missing refusal');
} catch (RpErrorRefusal $e) {
    assert($e->value->code === RpErrorCode::APPLICATION_USER_LIMIT);
}
try {
    $client->decryptToken(new Req(['a' => 'unknown']));
    throw new \LogicException('missing transport error');
} catch (\RuntimeException $e) {
    assert($e->getCode() === 2);
}
