<?php

namespace Csilgen\Transport;

/**
 * CSIL-RPC transport -- request/response/push envelopes -- see csil-rpc-transport.md.
 *
 * Payloads are opaque CBOR(type) bytes; on the wire they ride inside tag 24. Map
 * keys are deterministically ordered by CBOR::encode, so the same logical envelope
 * always yields the same bytes as the Rust/Go reference libraries.
 */
final class Rpc
{
    /**
     * Encode a CsilRpcRequest (spec section 1.1): {v, service, op, ?id, payload, ?auth}.
     * `id` is required on multiplexed carriers, omitted on one-in-flight carriers.
     */
    public static function encodeRequest($service, $op, $payload, $id = null, $auth = null)
    {
        $map = array(
            'v' => Conventions::VERSION,
            'service' => $service,
            'op' => $op,
            'payload' => Conventions::tag24($payload),
        );
        if ($id !== null) {
            $map['id'] = $id;
        }
        if ($auth !== null) {
            $map['auth'] = $auth;
        }
        return CBOR::encode($map);
    }

    /**
     * @return array{service:string,op:string,id:?int,payload:string,auth:?string}
     */
    public static function decodeRequest($bytes)
    {
        $m = Conventions::decodeEnvelope($bytes);
        Conventions::checkVersion(Conventions::getUint($m, 'v'));
        if (!isset($m['payload'])) {
            throw new MalformedException("missing 'payload'");
        }
        return array(
            'service' => Conventions::getText($m, 'service'),
            'op' => Conventions::getText($m, 'op'),
            'id' => Conventions::getUintOpt($m, 'id'),
            'payload' => Conventions::untag24($m['payload']),
            'auth' => Conventions::getTextOpt($m, 'auth'),
        );
    }

    /**
     * Encode a CsilRpcResponse (spec section 1.2): {v, ?id, status, ?variant, ?error, payload}.
     * `payload` is empty when `status` is non-zero, but stays present as an empty
     * tag-24 byte string so success and failure share one envelope shape.
     */
    public static function encodeResponse($status, $payload, $id = null, $variant = null, $error = null)
    {
        $map = array(
            'v' => Conventions::VERSION,
            'status' => $status,
            'payload' => Conventions::tag24($payload),
        );
        if ($id !== null) {
            $map['id'] = $id;
        }
        if ($variant !== null) {
            $map['variant'] = $variant;
        }
        if ($error !== null) {
            $map['error'] = $error;
        }
        return CBOR::encode($map);
    }

    /**
     * @return array{id:?int,status:int,variant:?string,error:?string,payload:string}
     */
    public static function decodeResponse($bytes)
    {
        $m = Conventions::decodeEnvelope($bytes);
        Conventions::checkVersion(Conventions::getUint($m, 'v'));
        // A non-zero-status response may omit the payload entirely.
        $payload = isset($m['payload']) ? Conventions::untag24($m['payload']) : '';
        return array(
            'id' => Conventions::getUintOpt($m, 'id'),
            'status' => Conventions::getInt($m, 'status'),
            'variant' => Conventions::getTextOpt($m, 'variant'),
            'error' => Conventions::getTextOpt($m, 'error'),
            'payload' => $payload,
        );
    }

    /**
     * Return the decoded response unchanged when its status is ok; otherwise throw
     * StatusException. Callers use this after decodeResponse to surface transport
     * failures distinctly from application errors (which ride as a status-0 typed
     * variant inside the payload).
     */
    public static function checkStatus(array $response)
    {
        if ($response['status'] !== Conventions::STATUS_OK) {
            throw new StatusException($response['status'], $response['error']);
        }
        return $response;
    }

    /**
     * Encode a CsilRpcPush (spec section 1.3): {v, service, event, payload}.
     */
    public static function encodePush($service, $event, $payload)
    {
        return CBOR::encode(array(
            'v' => Conventions::VERSION,
            'service' => $service,
            'event' => $event,
            'payload' => Conventions::tag24($payload),
        ));
    }

    /**
     * @return array{service:string,event:string,payload:string}
     */
    public static function decodePush($bytes)
    {
        $m = Conventions::decodeEnvelope($bytes);
        Conventions::checkVersion(Conventions::getUint($m, 'v'));
        if (!isset($m['payload'])) {
            throw new MalformedException("missing 'payload'");
        }
        return array(
            'service' => Conventions::getText($m, 'service'),
            'event' => Conventions::getText($m, 'event'),
            'payload' => Conventions::untag24($m['payload']),
        );
    }
}
