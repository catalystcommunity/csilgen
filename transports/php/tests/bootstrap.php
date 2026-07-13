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
