<?php

namespace Csilgen\Transport;

class TransportException extends \RuntimeException {}
class CborException extends TransportException {}
class CarrierException extends TransportException {}
class MalformedException extends TransportException {}
class VersionException extends TransportException {}

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
