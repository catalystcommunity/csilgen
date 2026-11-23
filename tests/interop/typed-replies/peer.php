<?php
namespace Csilgen\Generated;

require getenv('CSILGEN_ROOT') . '/transports/php/src/Errors.php';
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

/** A fake transport: the generated client must pick the reply arm from the
 * variant this seam returns, with no help from the resolver. */
class Carrier implements CsilReplyTransport
{
    public function call($service, $op, $payload)
    {
        throw new \LogicException('multi-arm ops must use callReply');
    }

    public function callReply($service, $op, $payload)
    {
        $a = Codec::decodeReq($payload)->a;
        if ($a === 'unknown') {
            throw new \RuntimeException('unknown operation', 2);
        }
        if ($a === 'other') {
            return ['variant' => 'Other', 'payload' => Codec::encodeResp(new Resp(['b' => 'other']))];
        }
        if ($a === 'none') {
            return ['variant' => null, 'payload' => Codec::encodeResp(new Resp(['b' => 'none']))];
        }
        $reply = (new RpRouter(new Handler()))->dispatch($op, $payload);
        if ($reply['variant'] === 'RpError') assert(ord($reply['payload'][6]) === 7);
        return $reply;
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
foreach (['other', 'none'] as $bad) {
    try {
        $client->decryptToken(new Req(['a' => $bad]));
        throw new \LogicException("accepted reply with $bad variant");
    } catch (CodecException $e) {
        // An unknown or missing variant is a decode error.
    }
}
echo "typed replies pass\n";
