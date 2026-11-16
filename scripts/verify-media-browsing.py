#!/usr/bin/env python3
"""End-to-end checks for the browsing/media routes used by the Android app.

Starts a disposable release instance and verifies:

* `GET /api/files/media` lists images/videos and supports kind filters,
* image thumbnails are generated and search results expose `has_thumb`,
* video thumbnails are generated when `ffmpeg` is available,
* full-scope tokens can read the vault, while backup-scope tokens cannot and
  only see their `Phone Backups` subtree.

Run from the repository root with a built release binary:

    python3 scripts/verify-media-browsing.py --binary backend/target/release/nimbo
"""
from __future__ import annotations

import argparse
import base64
import http.cookiejar
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
FAILURES: list[str] = []
PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
)
# Small but structurally plausible MP4 for listing/filter checks when ffmpeg is
# unavailable. Thumbnail assertions are skipped in that case.
FAKE_MP4 = (
    b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom"
    b"\x00\x00\x00\x08free"
)


def check(condition: object, message: str) -> None:
    if not condition:
        FAILURES.append(message)
        print(f"FAIL: {message}", file=sys.stderr)


class Client:
    def __init__(self, base: str, token: str | None = None):
        self.base = base
        self.token = token
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.jar)
        )

    def request(self, method: str, path: str, payload: object | None = None, body=None, ctype=None):
        if payload is not None:
            body = json.dumps(payload).encode()
            ctype = "application/json"
        request = urllib.request.Request(self.base + path, data=body, method=method)
        if ctype:
            request.add_header("Content-Type", ctype)
        if self.token:
            request.add_header("Authorization", f"Bearer {self.token}")
        try:
            with self.opener.open(request, timeout=60) as response:
                return response.status, {
                    key.lower(): value for key, value in response.headers.items()
                }, response.read()
        except urllib.error.HTTPError as error:
            return error.code, {
                key.lower(): value for key, value in error.headers.items()
            }, error.read()

    def json(self, method: str, path: str, payload: object | None = None):
        status, _, body = self.request(method, path, payload=payload)
        try:
            parsed = json.loads(body or b"{}")
        except Exception:
            parsed = {}
        return status, parsed if isinstance(parsed, dict) else {}


def multipart(name: str, content: bytes) -> tuple[bytes, str]:
    boundary = "----nimbo-media-" + os.urandom(12).hex()
    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{name}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode() + content + f"\r\n--{boundary}--\r\n".encode()
    return body, f"multipart/form-data; boundary={boundary}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="backend/target/release/nimbo")
    args = parser.parse_args()
    binary = pathlib.Path(args.binary)
    if not binary.is_absolute():
        binary = ROOT / binary
    if not binary.is_file():
        print(f"missing release binary: {binary}", file=sys.stderr)
        return 2

    has_ffmpeg = shutil.which("ffmpeg") is not None
    ffmpeg_note = "" if has_ffmpeg else " (ffmpeg absent; video thumbnail check skipped)"

    test_root = pathlib.Path(os.environ.get("NIMBO_TEST_TMP", "/tmp/opencode"))
    test_root.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="nimbo-media-", dir=test_root))
    log = None
    process = None
    try:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        setup_token = "media-setup-" + os.urandom(16).hex()
        env = os.environ.copy()
        env.update(
            NIMBO_BIND=f"127.0.0.1:{port}",
            NIMBO_DATA_DIR=str(work / "data"),
            NIMBO_SETUP_TOKEN=setup_token,
            NIMBO_SECURE_COOKIES="false",
            NIMBO_MAX_UPLOAD_MB="16",
            RUST_LOG="nimbo=warn",
        )
        log = (work / "server.log").open("wb")
        process = subprocess.Popen(
            [str(binary)], cwd=ROOT / "backend", env=env,
            stdout=log, stderr=subprocess.STDOUT,
        )
        client = Client(base)
        for _ in range(300):
            if process.poll() is not None:
                raise RuntimeError(
                    "server exited early:\n"
                    + (work / "server.log").read_text(errors="replace")[-4000:]
                )
            try:
                if client.request("GET", "/healthz")[0] == 200:
                    break
            except Exception:
                pass
            time.sleep(0.05)
        else:
            raise RuntimeError("server did not become healthy")

        status, _ = client.json("POST", "/api/setup", {
            "instance_name": "Media", "username": "owner",
            "password": "review-password", "setup_token": setup_token,
        })
        check(status == 201, f"setup succeeds ({status})")
        status, data = client.json("POST", "/api/tokens", {"name": "full", "scope": "full"})
        check(status == 200 and data.get("token"), "full-scope token is created")
        full_token = data.get("token")
        status, data = client.json("POST", "/api/tokens", {"name": "backup", "scope": "backup"})
        check(status == 200 and data.get("token"), "backup-scope token is created")
        backup_token = data.get("token")

        if has_ffmpeg:
            clip = work / "clip.mp4"
            subprocess.run(
                ["ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
                 "testsrc=size=160x120:rate=10", "-t", "1",
                 "-pix_fmt", "yuv420p", str(clip)],
                check=True,
            )
            video_bytes = clip.read_bytes()
        else:
            video_bytes = FAKE_MP4

        ids: dict[str, str] = {}
        for name, content in [("photo.png", PNG), ("clip.mp4", video_bytes)]:
            body, ctype = multipart(name, content)
            status, _, raw = client.request("POST", "/api/files/upload", body=body, ctype=ctype)
            data = json.loads(raw or b"{}")
            check(status == 200 and data.get("uploaded"), f"upload {name} ({status})")
            if data.get("uploaded"):
                ids[name] = data["uploaded"][0]["id"]

        deadline = time.time() + 30
        items: list[dict] = []
        while time.time() < deadline:
            status, data = client.json("GET", "/api/files/media?kind=all")
            items = data.get("items", []) if status == 200 else []
            image_ready = any(
                item.get("name") == "photo.png" and item.get("has_thumb") for item in items
            )
            video_ready = not has_ffmpeg or any(
                item.get("name") == "clip.mp4" and item.get("has_thumb") for item in items
            )
            if image_ready and video_ready and len(items) == 2:
                break
            time.sleep(0.25)

        check(status == 200, f"media endpoint responds ({status})")
        by_name = {item.get("name"): item for item in items}
        check(len(items) == 2, f"media lists the image and video (got {len(items)})")
        check(by_name.get("photo.png", {}).get("has_thumb") is True, "image has a thumbnail")
        check(by_name.get("photo.png", {}).get("path") == "photo.png",
              "media result includes its path")
        if has_ffmpeg:
            check(by_name.get("clip.mp4", {}).get("has_thumb") is True,
                  "video has an ffmpeg thumbnail")

        status, data = client.json("GET", "/api/files/media?kind=image")
        check(status == 200 and [i["name"] for i in data.get("items", [])] == ["photo.png"],
              "image filter returns only images")
        status, data = client.json("GET", "/api/files/media?kind=video")
        check(status == 200 and [i["name"] for i in data.get("items", [])] == ["clip.mp4"],
              "video filter returns only videos")
        status, _ = client.json("GET", "/api/files/media?kind=bogus")
        check(status == 400, f"unknown media kind is rejected ({status})")

        status, data = client.json("GET", "/api/files/search?q=photo")
        results = data.get("results", [])
        check(status == 200 and any(
            item.get("name") == "photo.png" and item.get("has_thumb")
            for item in results
        ), "search results expose thumbnail availability")

        status, headers, body = client.request(
            "GET", f"/api/files/{ids['photo.png']}/thumbnail"
        )
        check(status == 200 and headers.get("content-type") == "image/jpeg" and body,
              "thumbnail endpoint serves JPEG bytes")
        if has_ffmpeg:
            status, headers, body = client.request(
                "GET", f"/api/files/{ids['clip.mp4']}/thumbnail"
            )
            check(status == 200 and headers.get("content-type") == "image/jpeg" and body,
                  "video thumbnail endpoint serves JPEG bytes")

        status, _ = client.json("PUT", "/api/docs/content", {
            "path": "notes/welcome.md", "content": "# Welcome\n\nHello.\n",
        })
        check(status == 200, f"vault save succeeds ({status})")
        status, data = client.json("GET", "/api/docs/tree")
        check(status == 200 and any(
            doc.get("path") == "notes/welcome.md" for doc in data.get("documents", [])
        ), "vault tree lists documents")
        status, data = client.json("GET", "/api/docs/content?path=notes%2Fwelcome.md")
        check(status == 200 and data.get("content", "").startswith("# Welcome"),
              "vault content is readable")

        backup = Client(base, backup_token)
        status, _ = backup.json("GET", "/api/docs/tree")
        check(status == 403, f"backup token cannot read the vault ({status})")
        status, data = backup.json("GET", "/api/files/media?kind=all")
        check(status == 200 and data.get("items", []) == [],
              "backup token sees no media outside Phone Backups")
        status, _ = backup.json("GET", "/api/search?q=welcome")
        check(status == 403, f"backup token cannot use vault search ({status})")

        full = Client(base, full_token)
        status, data = full.json("GET", "/api/files/media?kind=all")
        check(status == 200 and len(data.get("items", [])) == 2,
              "full bearer token browses all media")
        status, _ = full.json("GET", "/api/docs/tree")
        check(status == 200, "full bearer token browses the vault")
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
        if log is not None:
            log.close()
        if FAILURES:
            print(f"logs kept at {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)

    if FAILURES:
        print(f"{len(FAILURES)} failure(s){ffmpeg_note}")
        return 1
    print(f"MEDIA_BROWSING_OK{ffmpeg_note}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
