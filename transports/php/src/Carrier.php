<?php

namespace Csilgen\Transport;

interface ByteCarrier
{
    public function send($bytes);
    public function receive();
}

final class InMemoryCarrier implements ByteCarrier
{
    /** @var array<int,string> */
    private $queue = array();

    public function send($bytes)
    {
        $this->queue[] = $bytes;
    }

    public function receive()
    {
        if (!$this->queue) {
            throw new CarrierException('empty in-memory carrier');
        }
        return array_shift($this->queue);
    }
}
