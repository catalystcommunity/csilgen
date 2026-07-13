<?php

namespace Csilgen\Transport;

class TransportException extends \RuntimeException {}
class CborException extends TransportException {}
class CarrierException extends TransportException {}
class StatusException extends TransportException {}
