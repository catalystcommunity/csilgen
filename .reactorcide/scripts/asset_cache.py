"""Small S3 asset-cache client for trusted Reactorcide CI code."""

from __future__ import annotations

import datetime as dt
import hashlib
import hmac
import json
import os
import re
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


PROJECT = "csilgen"
MANIFEST = "complete.json"
LANE_RE = re.compile(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,159}$")
VERSION_LANE_RE = re.compile(
    r"^v(?P<major>0|[1-9][0-9]*)\."
    r"(?P<minor>0|[1-9][0-9]*)\."
    r"(?P<patch>0|[1-9][0-9]*)"
    r"(?P<suffix>-[0-9A-Za-z.-]+)?$"
)


@dataclass(frozen=True)
class ObjectInfo:
    """Describe one object without its contents."""

    key: str
    size: int
    last_modified: dt.datetime


class S3Cache:
    """Read and write one S3-compatible bucket with Signature Version 4."""

    def __init__(
        self,
        endpoint: str,
        bucket: str,
        access_key: str,
        secret_key: str,
        *,
        region: str = "us-east-1",
    ) -> None:
        parsed = urllib.parse.urlsplit(endpoint.rstrip("/"))
        if parsed.scheme not in {"http", "https"} or not parsed.netloc:
            raise RuntimeError("The asset-cache endpoint is invalid")
        if not re.fullmatch(r"[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]", bucket):
            raise RuntimeError("The asset-cache bucket name is invalid")
        if not access_key or not secret_key:
            raise RuntimeError("The asset-cache credentials are missing")
        self.endpoint = endpoint.rstrip("/")
        self.bucket = bucket
        self.access_key = access_key
        self.secret_key = secret_key
        self.region = region
        self.host = parsed.netloc

    @classmethod
    def from_environment(cls) -> "S3Cache":
        """Create a client without putting credential values in arguments."""

        required = {
            "endpoint": os.environ.get("ASSET_CACHE_ENDPOINT", ""),
            "bucket": os.environ.get("ASSET_CACHE_BUCKET", ""),
            "access_key": os.environ.get("ASSET_CACHE_ACCESS_KEY", ""),
            "secret_key": os.environ.get("ASSET_CACHE_SECRET_KEY", ""),
        }
        missing = sorted(name for name, value in required.items() if not value)
        if missing:
            raise RuntimeError(
                "The asset-cache environment is missing: " + ", ".join(missing)
            )
        return cls(**required)

    def _request(
        self,
        method: str,
        key: str = "",
        *,
        query: Iterable[tuple[str, str]] = (),
        body: bytes = b"",
        headers: dict[str, str] | None = None,
    ) -> bytes:
        clean_key = key.lstrip("/")
        path = f"/{self.bucket}"
        if clean_key:
            path += "/" + urllib.parse.quote(clean_key, safe="/-_.~")
        sorted_query = sorted(query)
        canonical_query = urllib.parse.urlencode(sorted_query, quote_via=urllib.parse.quote)
        request_url = self.endpoint + path
        if canonical_query:
            request_url += "?" + canonical_query

        now = dt.datetime.now(dt.timezone.utc)
        amz_date = now.strftime("%Y%m%dT%H%M%SZ")
        date_stamp = now.strftime("%Y%m%d")
        payload_hash = hashlib.sha256(body).hexdigest()
        signed_headers = {
            "host": self.host,
            "x-amz-content-sha256": payload_hash,
            "x-amz-date": amz_date,
        }
        for name, value in (headers or {}).items():
            signed_headers[name.lower()] = " ".join(value.strip().split())
        header_names = ";".join(sorted(signed_headers))
        canonical_headers = "".join(
            f"{name}:{signed_headers[name]}\n" for name in sorted(signed_headers)
        )
        canonical_request = "\n".join(
            (
                method,
                path,
                canonical_query,
                canonical_headers,
                header_names,
                payload_hash,
            )
        )
        scope = f"{date_stamp}/{self.region}/s3/aws4_request"
        string_to_sign = "\n".join(
            (
                "AWS4-HMAC-SHA256",
                amz_date,
                scope,
                hashlib.sha256(canonical_request.encode()).hexdigest(),
            )
        )
        signing_key = _signing_key(self.secret_key, date_stamp, self.region)
        signature = hmac.new(
            signing_key,
            string_to_sign.encode(),
            hashlib.sha256,
        ).hexdigest()
        authorization = (
            "AWS4-HMAC-SHA256 "
            f"Credential={self.access_key}/{scope},"
            f"SignedHeaders={header_names},Signature={signature}"
        )
        request = urllib.request.Request(request_url, data=body, method=method)
        for name, value in signed_headers.items():
            request.add_header(name, value)
        request.add_header("Authorization", authorization)
        for attempt in range(5):
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    return response.read()
            except urllib.error.HTTPError as error:
                if error.code == 404:
                    raise FileNotFoundError(key) from None
                if error.code not in {408, 429, 500, 502, 503, 504} or attempt == 4:
                    raise RuntimeError(
                        f"Asset-cache request failed with HTTP {error.code}"
                    ) from None
            except (urllib.error.URLError, TimeoutError):
                if attempt == 4:
                    raise RuntimeError("The asset-cache request failed") from None
            time.sleep(2**attempt)
        raise RuntimeError("The asset-cache request failed")

    def presign(self, method: str, key: str, *, expires: int = 43200) -> str:
        """Create an exact-object URL without exposing the secret key."""

        if method not in {"GET", "PUT"}:
            raise RuntimeError("The presigned asset-cache method is invalid")
        if expires < 1 or expires > 604800:
            raise RuntimeError("The presigned asset-cache lifetime is invalid")
        clean_key = key.lstrip("/")
        path = f"/{self.bucket}/{urllib.parse.quote(clean_key, safe='/-_.~')}"
        now = dt.datetime.now(dt.timezone.utc)
        amz_date = now.strftime("%Y%m%dT%H%M%SZ")
        date_stamp = now.strftime("%Y%m%d")
        scope = f"{date_stamp}/{self.region}/s3/aws4_request"
        query = [
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256"),
            ("X-Amz-Credential", f"{self.access_key}/{scope}"),
            ("X-Amz-Date", amz_date),
            ("X-Amz-Expires", str(expires)),
            ("X-Amz-SignedHeaders", "host"),
        ]
        canonical_query = urllib.parse.urlencode(
            sorted(query), quote_via=urllib.parse.quote
        )
        canonical_request = "\n".join(
            (
                method,
                path,
                canonical_query,
                f"host:{self.host}\n",
                "host",
                "UNSIGNED-PAYLOAD",
            )
        )
        string_to_sign = "\n".join(
            (
                "AWS4-HMAC-SHA256",
                amz_date,
                scope,
                hashlib.sha256(canonical_request.encode()).hexdigest(),
            )
        )
        signature = hmac.new(
            _signing_key(self.secret_key, date_stamp, self.region),
            string_to_sign.encode(),
            hashlib.sha256,
        ).hexdigest()
        return (
            self.endpoint
            + path
            + "?"
            + canonical_query
            + "&X-Amz-Signature="
            + signature
        )

    def put_bytes(self, key: str, content: bytes) -> None:
        self._request("PUT", key, body=content)

    def put_file(self, key: str, source: Path) -> None:
        self.put_bytes(key, source.read_bytes())

    def get_bytes(self, key: str) -> bytes:
        return self._request("GET", key)

    def get_file(self, key: str, destination: Path) -> None:
        content = self.get_bytes(key)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)

    def copy(self, source_key: str, destination_key: str) -> None:
        encoded = urllib.parse.quote(f"/{self.bucket}/{source_key}", safe="/-_.~")
        self._request(
            "PUT",
            destination_key,
            headers={"x-amz-copy-source": encoded},
        )

    def delete(self, key: str) -> None:
        self._request("DELETE", key)

    def list(self, prefix: str) -> list[ObjectInfo]:
        objects: list[ObjectInfo] = []
        continuation = ""
        while True:
            query = [("list-type", "2"), ("prefix", prefix)]
            if continuation:
                query.append(("continuation-token", continuation))
            root = ET.fromstring(self._request("GET", query=query))
            namespace = ""
            if root.tag.startswith("{"):
                namespace = root.tag.split("}", 1)[0] + "}"
            for item in root.findall(f"{namespace}Contents"):
                key = item.findtext(f"{namespace}Key", default="")
                size = int(item.findtext(f"{namespace}Size", default="0"))
                modified = item.findtext(f"{namespace}LastModified", default="")
                if key and modified:
                    objects.append(
                        ObjectInfo(
                            key=key,
                            size=size,
                            last_modified=dt.datetime.fromisoformat(
                                modified.replace("Z", "+00:00")
                            ),
                        )
                    )
            truncated = root.findtext(f"{namespace}IsTruncated", default="false")
            if truncated.lower() != "true":
                break
            continuation = root.findtext(
                f"{namespace}NextContinuationToken",
                default="",
            )
            if not continuation:
                raise RuntimeError("The asset-cache listing has no continuation token")
        return objects


def _signing_key(secret: str, date_stamp: str, region: str) -> bytes:
    date_key = hmac.new(
        ("AWS4" + secret).encode(), date_stamp.encode(), hashlib.sha256
    ).digest()
    region_key = hmac.new(date_key, region.encode(), hashlib.sha256).digest()
    service_key = hmac.new(region_key, b"s3", hashlib.sha256).digest()
    return hmac.new(service_key, b"aws4_request", hashlib.sha256).digest()


def object_key(lane: str, asset: str) -> str:
    """Return one validated project/lane/asset object key."""

    if not LANE_RE.fullmatch(lane):
        raise RuntimeError(f"The asset-cache lane is invalid: {lane!r}")
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,199}", asset):
        raise RuntimeError(f"The asset-cache asset is invalid: {asset!r}")
    return f"{PROJECT}/{lane}/{asset}"


def pr_lane(pr_number: str, source_sha: str) -> str:
    """Return a collision-resistant lane for one exact PR revision."""

    if not pr_number.isdigit() or not re.fullmatch(r"[0-9a-fA-F]{12,64}", source_sha):
        raise RuntimeError("The PR lane inputs are invalid")
    return f"pr-{int(pr_number)}-{source_sha[:12].lower()}"


def main_lane(source_sha: str) -> str:
    """Return the immutable main lane for one merge commit."""

    if not re.fullmatch(r"[0-9a-fA-F]{12,64}", source_sha):
        raise RuntimeError("The main lane commit is invalid")
    return f"main-{source_sha[:12].lower()}"


def version_lane(version: str) -> str:
    """Return the immutable lane for one release version."""

    lane = "v" + version
    if not VERSION_LANE_RE.fullmatch(lane):
        raise RuntimeError("The release version is invalid")
    return lane


def file_sha256(path: Path) -> str:
    """Calculate a file digest without loading the file into a log."""

    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def encode_manifest(value: dict[str, Any]) -> bytes:
    """Encode a stable manifest."""

    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def decode_manifest(content: bytes) -> dict[str, Any]:
    """Decode and validate the outer manifest shape."""

    value = json.loads(content)
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise RuntimeError("The asset-cache manifest is invalid")
    return value


def version_sort_key(lane: str) -> tuple[int, int, int, int, str]:
    """Return a stable key that puts releases after prereleases."""

    match = VERSION_LANE_RE.fullmatch(lane)
    if not match:
        raise RuntimeError(f"The version lane is invalid: {lane}")
    suffix = match.group("suffix") or ""
    return (
        int(match.group("major")),
        int(match.group("minor")),
        int(match.group("patch")),
        1 if not suffix else 0,
        suffix,
    )
