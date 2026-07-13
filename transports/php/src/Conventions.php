<?php

namespace Csilgen\Transport;

final class Conventions
{
    public static function methodName($service, $operation)
    {
        return self::kebab($service) . '/' . self::kebab($operation);
    }

    private static function kebab($name)
    {
        $name = preg_replace('/([a-z0-9])([A-Z])/', '$1-$2', $name);
        $name = preg_replace('/[^A-Za-z0-9]+/', '-', $name);
        return strtolower(trim($name, '-'));
    }
}
