# csilgen PHP transport

Pure PHP 7.2+ reference implementation of the CSIL transport envelopes:
CSIL-RPC, CSIL-Events, CSIL-Datagrams, and canonical CBOR helpers.

The package is Composer-shaped as `csilgen/transport`. Until it is published,
consume it from a git checkout:

```json
{
  "repositories": [
    { "type": "path", "url": "path/to/csilgen/transports/php" }
  ],
  "require": {
    "csilgen/transport": "*"
  }
}
```

Run tests with:

```bash
cd transports/php
./run-tests.sh
```

For repo-local testing, `tools/install-transport-toolchains.sh` can build a
self-contained PHP 8.x CLI under `~/.config/catalyst-tools` via static-php-cli
and install Composer beside it. Exact PHP 7.x runtime testing still needs a
system/package-manager PHP 7.x, phpbrew/asdf, or a source build; the generated
code intentionally stays within PHP 7.2-compatible syntax.
