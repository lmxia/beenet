#!/usr/bin/env python3
"""Upload release assets to Aliyun OSS under releases/<tag>/ and releases/latest/.

Bucket and endpoint are repository config (not GitHub Secrets). Override with
OSS_BUCKET / OSS_ENDPOINT / OSS_PREFIX if needed. Credentials come from
OSS_ACCESS_KEY_ID and OSS_ACCESS_KEY_SECRET.
"""

from __future__ import annotations

import mimetypes
import os
import sys
from pathlib import Path

import oss2

DEFAULT_ENDPOINT = "https://oss-cn-hangzhou.aliyuncs.com"
DEFAULT_BUCKET = "beenet-hyperos-prod"
DEFAULT_PREFIX = "releases"

CONTENT_TYPES = {
    ".exe": "application/x-msdownload",
    ".dmg": "application/x-apple-diskimage",
    ".gz": "application/gzip",
    ".sh": "application/x-shellscript",
}


def content_type(path: Path) -> str:
    if path.name.endswith(".tar.gz"):
        return "application/gzip"
    return CONTENT_TYPES.get(path.suffix, mimetypes.guess_type(path.name)[0] or "application/octet-stream")


def main() -> int:
    if len(sys.argv) < 3:
        print("usage: oss-upload-release.py <tag> <file> [file...]", file=sys.stderr)
        return 2

    access_key_id = os.environ.get("OSS_ACCESS_KEY_ID", "").strip()
    access_key_secret = os.environ.get("OSS_ACCESS_KEY_SECRET", "").strip()
    if not access_key_id or not access_key_secret:
        print("OSS_ACCESS_KEY_ID and OSS_ACCESS_KEY_SECRET are required", file=sys.stderr)
        return 1

    endpoint = os.environ.get("OSS_ENDPOINT", DEFAULT_ENDPOINT).strip() or DEFAULT_ENDPOINT
    bucket_name = os.environ.get("OSS_BUCKET", DEFAULT_BUCKET).strip() or DEFAULT_BUCKET
    prefix = os.environ.get("OSS_PREFIX", DEFAULT_PREFIX).strip().strip("/") or DEFAULT_PREFIX
    tag = sys.argv[1].strip()
    files = [Path(item) for item in sys.argv[2:]]

    missing = [str(path) for path in files if not path.is_file()]
    if missing:
        print("missing files: " + ", ".join(missing), file=sys.stderr)
        return 1

    bucket = oss2.Bucket(oss2.Auth(access_key_id, access_key_secret), endpoint, bucket_name)
    for path in files:
        headers = {"Content-Type": content_type(path)}
        for key in (f"{prefix}/{tag}/{path.name}", f"{prefix}/latest/{path.name}"):
            oss2.resumable_upload(bucket, key, str(path), headers=headers)
            print(f"uploaded oss://{bucket_name}/{key} ({path.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
