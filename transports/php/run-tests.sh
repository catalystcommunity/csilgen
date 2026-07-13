#!/usr/bin/env bash
set -euo pipefail

if ! command -v php >/dev/null 2>&1; then
  echo "PHP not found; skipping PHP transport tests."
  exit 0
fi

php tests/conformance_test.php
php tests/roundtrip_test.php
