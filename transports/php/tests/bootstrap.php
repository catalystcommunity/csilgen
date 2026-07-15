<?php

require_once __DIR__ . '/../src/Errors.php';
require_once __DIR__ . '/../src/CBOR.php';
require_once __DIR__ . '/../src/Conventions.php';
require_once __DIR__ . '/../src/Rpc.php';
require_once __DIR__ . '/../src/Events.php';
require_once __DIR__ . '/../src/Datagrams.php';
require_once __DIR__ . '/../src/Carrier.php';

function assert_true($cond, $message)
{
    if (!$cond) {
        fwrite(STDERR, "assertion failed: " . $message . "\n");
        exit(1);
    }
}

function assert_throws($class, $fn, $message)
{
    try {
        $fn();
    } catch (\Exception $e) {
        assert_true($e instanceof $class, $message . ' (threw ' . get_class($e) . ')');
        return $e;
    }
    assert_true(false, $message . ' (nothing thrown)');
}
