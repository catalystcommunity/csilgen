<?php

namespace Csilgen\Transport;

class TransportException extends \RuntimeException {}
class CborException extends TransportException {}
class CarrierException extends TransportException {}
class MalformedException extends TransportException {}
class VersionException extends TransportException {}

/** A frame exceeded the max-frame guard; rejected before allocating for it. */
class FrameTooLargeException extends TransportException
{
    /** @var int */
    private $got;
    /** @var int */
    private $maximum;

    public function __construct($got, $maximum)
    {
        $this->got = $got;
        $this->maximum = $maximum;
        parent::__construct(
            'frame of ' . $got . ' bytes exceeds max-frame guard of ' . $maximum . ' bytes'
        );
    }

    public function getGot()
    {
        return $this->got;
    }

    public function getMaximum()
    {
        return $this->maximum;
    }
}

/** A host configured a max-frame limit outside the valid range. */
class InvalidMaxFrameException extends TransportException
{
    /** @var mixed */
    private $got;
    /** @var int */
    private $limit;

    public function __construct($got, $limit)
    {
        $this->got = $got;
        $this->limit = $limit;
        parent::__construct(
            'max-frame limit of ' . var_export($got, true)
            . ' is outside the valid range 1..=' . $limit
        );
    }

    public function getGot()
    {
        return $this->got;
    }

    public function getLimit()
    {
        return $this->limit;
    }
}

/** A non-zero transport status returned by the peer, distinct from application errors. */
class StatusException extends TransportException
{
    /** @var int */
    private $statusCode;

    public function __construct($statusCode, $error = null)
    {
        $this->statusCode = $statusCode;
        $name = Conventions::statusName($statusCode);
        $message = 'transport status ' . $name . ' (' . $statusCode . ')';
        if ($error !== null) {
            $message .= ': ' . $error;
        }
        parent::__construct($message);
    }

    public function getStatusCode()
    {
        return $this->statusCode;
    }

    public function getStatusName()
    {
        return Conventions::statusName($this->statusCode);
    }
}
