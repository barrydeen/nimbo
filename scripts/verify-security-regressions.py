#!/usr/bin/env python3
"""End-to-end security regression checks against a disposable Nimbo instance.

These checks are intentionally destructive only inside a temporary data directory.
They exercise the previously reproduced document/tree races, HTML snippet sink,
symlink handling, HTTP Range behavior, backup-token containment, and replacement
API against the real release binary.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import http.cookiejar
import json
import os
import pathlib
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass

ROOT = pathlib.Path(__file__).resolve().parent.parent
FAILURES: list[str] = []


def check(condition: object, message: str) -> None:
    if not condition:
        FAILURES.append(message)
        print(f"FAIL: {message}", file=sys.stderr)


@dataclass
class Reply:
    status: int
    headers: dict[str, str]
    body: bytes

    def json(self) -> dict:
        try:
            value = json.loads(self.body) if self.body else {}
            return value if isinstance(value, dict) else {}
        except Exception:
            return {}


class Client:
    def __init__(self, base: str, token: str | None = None):
        self.base = base
        self.token = token
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.cookies))

    def request(
        self,
        method: str,
        path: str,
        *,
        payload: object | None = None,
        body: bytes | None = None,
        content_type: str | None = None,
        headers: dict[str, str] | None = None,
    ) -> Reply:
        if payload is not None:
            body = json.dumps(payload).encode()
            content_type = "application/json"
        req = urllib.request.Request(self.base + path, data=body, method=method)
        if content_type:
            req.add_header("Content-Type", content_type)
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        for key, value in (headers or {}).items():
            req.add_header(key, value)
        try:
            with self.opener.open(req, timeout=60) as response:
                return Reply(response.status, {k.lower(): v for k, v in response.headers.items()}, response.read())
        except urllib.error.HTTPError as error:
            return Reply(error.code, {k.lower(): v for k, v in error.headers.items()}, error.read())


def multipart(name: str, content: bytes) -> tuple[bytes, str]:
    boundary = "----nimbo-regression-" + os.urandom(12).hex()
    safe_name = name.replace('"', "_").replace("\r", "_").replace("\n", "_")
    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{safe_name}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode() + content + f"\r\n--{boundary}--\r\n".encode()
    return body, f"multipart/form-data; boundary={boundary}"


def wait_ready(client: Client, process: subprocess.Popen[bytes], log_path: pathlib.Path) -> None:
    for _ in range(300):
        if process.poll() is not None:
            output = log_path.read_text(errors="replace") if log_path.exists() else ""
            raise RuntimeError(f"server exited early ({process.returncode}):\n{output[-4000:]}")
        try:
            if client.request("GET", "/healthz").status == 200:
                return
        except Exception:
            pass
        time.sleep(0.05)
    raise RuntimeError("server did not become healthy")


def body_text(reply: Reply) -> str:
    return reply.body.decode(errors="replace")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="backend/target/release/nimbo")
    parser.add_argument("--keep-data", action="store_true")
    args = parser.parse_args()

    binary = pathlib.Path(args.binary)
    if not binary.is_absolute():
        binary = ROOT / binary
    if not binary.is_file():
        print(f"missing release binary: {binary}", file=sys.stderr)
        return 2

    test_tmp = pathlib.Path(os.environ.get("NIMBO_TEST_TMP", "/tmp/opencode"))
    test_tmp.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="nimbo-security-regressions-", dir=test_tmp))
    log_path = work / "server.log"
    process: subprocess.Popen[bytes] | None = None
    log = None
    try:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        setup_token = "regression-setup-" + os.urandom(16).hex()
        env = os.environ.copy()
        env.update(
            NIMBO_BIND=f"127.0.0.1:{port}",
            NIMBO_DATA_DIR=str(work / "data"),
            NIMBO_SETUP_TOKEN=setup_token,
            NIMBO_SECURE_COOKIES="false",
            NIMBO_MAX_UPLOAD_MB="8",
            RUST_LOG="nimbo=warn",
        )
        log = log_path.open("wb")
        process = subprocess.Popen(
            [str(binary)], cwd=ROOT / "backend", env=env, stdout=log, stderr=subprocess.STDOUT
        )
        client = Client(base)
        wait_ready(client, process, log_path)

        # Fresh setup requires the bootstrap token and rejects completed setup cheaply.
        bootstrap = client.request("GET", "/api/bootstrap")
        check(bootstrap.status == 200, "bootstrap is available")
        check(bootstrap.json().get("setup_token_required") is True, "bootstrap advertises required setup token")
        missing = client.request(
            "POST", "/api/setup",
            payload={"instance_name": "Regression", "username": "owner", "password": "review-password"},
        )
        check(missing.status in (401, 403), "setup rejects a missing bootstrap token")
        wrong = client.request(
            "POST", "/api/setup",
            payload={"instance_name": "Regression", "username": "owner", "password": "review-password", "setup_token": "wrong"},
        )
        check(wrong.status in (401, 403), "setup rejects a wrong bootstrap token")
        created = client.request(
            "POST", "/api/setup",
            payload={"instance_name": "Regression", "username": "owner", "password": "review-password", "setup_token": setup_token},
        )
        check(created.status == 201, f"valid bootstrap setup succeeds (got {created.status})")
        repeated = client.request(
            "POST", "/api/setup",
            payload={"instance_name": "Again", "username": "other", "password": "review-password", "setup_token": setup_token},
        )
        check(repeated.status in (400, 403, 409, 410), "completed setup rejects repeat enrollment")
        oversized_login = client.request(
            "POST", "/api/auth/login", payload={"username": "x" * 20_000, "password": "x"}
        )
        check(oversized_login.status in (400, 413), "oversized login usernames are rejected before limiter retention")
        cross_origin = client.request(
            "POST", "/api/files/folders",
            payload={"parent_id": None, "name": "should-not-exist"},
            headers={"Origin": "https://attacker.example"},
        )
        check(cross_origin.status == 403, "cross-origin authenticated mutation is rejected")

        # Search snippets escape source markup and retain only intended marks.
        evil = client.request(
            "PUT", "/api/docs/content",
            payload={"path": "evil.md", "content": "# Evil\n\nnarwhal <img src=x onerror=alert(1)>", "base_hash": None},
        )
        check(evil.status == 200, "malicious test note can be saved")
        search = client.request("GET", "/api/search?q=narwhal")
        check(search.status == 200, "document search succeeds")
        snippets = [str(row.get("snippet", "")) for row in search.json().get("results", [])]
        combined = "\n".join(snippets)
        check("<img" not in combined.lower(), "document snippet never emits a live image tag")
        check("&lt;img" in combined.lower(), "document snippet HTML-escapes source markup")
        check("<mark>" in combined, "document snippet preserves intended highlights")

        # Symlinked vault entries cannot read outside the vault.
        secret = work / "host-secret.txt"
        secret.write_text("HOST_SECRET_MUST_NOT_LEAK")
        vault = work / "data" / "vault"
        vault.mkdir(parents=True, exist_ok=True)
        symlink = vault / "leak.md"
        symlink.symlink_to(secret)
        leaked = client.request("GET", "/api/docs/content?path=leak.md")
        check(leaked.status >= 400, "symlinked vault document is rejected")
        check("HOST_SECRET_MUST_NOT_LEAK" not in body_text(leaked), "symlink rejection does not expose host content")

        # Two cooperating saves from the same base serialize; no shared-temp 500.
        save_pairs: dict[tuple[int, int], int] = {}
        save_500 = 0
        for iteration in range(20):
            path = f"race-{iteration}.md"
            initial = client.request(
                "PUT", "/api/docs/content",
                payload={"path": path, "content": "base", "base_hash": None},
            )
            check(initial.status == 200, f"race seed {iteration} saves")
            base_hash = initial.json().get("hash")
            left = "L" * (256 * 1024) + f"-{iteration}"
            right = "R" * (256 * 1024) + f"-{iteration}"

            def update(content: str) -> Reply:
                return client.request(
                    "PUT", "/api/docs/content",
                    payload={"path": path, "content": content, "base_hash": base_hash},
                )

            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                a, b = pool.submit(update, left), pool.submit(update, right)
                pair = (a.result().status, b.result().status)
            save_pairs[pair] = save_pairs.get(pair, 0) + 1
            save_500 += pair.count(500)
            check(pair != (200, 200), f"same-base save pair {iteration} cannot have two winners: {pair}")
            current = client.request("GET", f"/api/docs/content?path={urllib.parse.quote(path)}")
            check(current.status == 200, f"race document {iteration} remains readable")
            check(current.json().get("content") in (left, right), f"race document {iteration} has one intact winner")
        check(save_500 == 0, f"serialized saves expose no shared-temp HTTP 500s ({save_500} found)")
        print("DOC_SAVE_STATUS_PAIRS", save_pairs)

        # Different-source/same-destination renames never both succeed.
        rename_pairs: dict[tuple[int, int], int] = {}
        rename_double = 0
        for iteration in range(40):
            left_path = f"rename-left-{iteration}.md"
            right_path = f"rename-right-{iteration}.md"
            destination = f"rename-dest-{iteration}.md"
            for source, content in ((left_path, f"LEFT-{iteration}"), (right_path, f"RIGHT-{iteration}")):
                check(
                    client.request("PUT", "/api/docs/content", payload={"path": source, "content": content, "base_hash": None}).status == 200,
                    f"rename seed {source} saves",
                )

            def rename(source: str) -> Reply:
                return client.request("POST", "/api/docs/rename", payload={"from": source, "to": destination})

            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                a, b = pool.submit(rename, left_path), pool.submit(rename, right_path)
                pair = (a.result().status, b.result().status)
            rename_pairs[pair] = rename_pairs.get(pair, 0) + 1
            if pair == (200, 200):
                rename_double += 1
            dest = client.request("GET", f"/api/docs/content?path={urllib.parse.quote(destination)}")
            check(dest.status == 200, f"rename destination {iteration} exists")
            check(
                dest.json().get("content") in (f"LEFT-{iteration}", f"RIGHT-{iteration}"),
                f"rename destination {iteration} is an intact complete document",
            )
        check(rename_double == 0, f"concurrent renames never both overwrite ({rename_double} double wins)")
        print("DOC_RENAME_STATUS_PAIRS", rename_pairs)

        # Two-folder move cycles are prevented.
        cycle_double = 0
        for iteration in range(30):
            folder_ids = []
            for side in ("left", "right"):
                reply = client.request("POST", "/api/files/folders", payload={"parent_id": None, "name": f"cycle-{side}-{iteration}"})
                check(reply.status == 200, f"cycle folder {side}-{iteration} creates")
                folder_ids.append(reply.json().get("item", {}).get("id"))
            left_id, right_id = folder_ids

            def move(item: str, parent: str) -> Reply:
                return client.request("PATCH", f"/api/files/{item}", payload={"parent_id": parent})

            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                a, b = pool.submit(move, left_id, right_id), pool.submit(move, right_id, left_id)
                pair = (a.result().status, b.result().status)
            if pair == (200, 200):
                cycle_double += 1
            db = sqlite3.connect(work / "data" / "nimbo.db")
            parents = dict(db.execute("SELECT id, parent_id FROM nodes WHERE id IN (?, ?)", (left_id, right_id)))
            db.close()
            check(
                not (parents.get(left_id) == right_id and parents.get(right_id) == left_id),
                f"concurrent moves cannot form a cycle at iteration {iteration}",
            )
        check(cycle_double == 0, f"concurrent folder swaps never both succeed ({cycle_double} double wins)")

        # Private folder listings are bounded and resumable instead of loading an
        # unbounded folder into one JSON response.
        paging_parent = client.request(
            "POST", "/api/files/folders", payload={"parent_id": None, "name": "pagination-proof"}
        )
        check(paging_parent.status == 200, "pagination parent folder creates")
        paging_id = paging_parent.json().get("item", {}).get("id")
        for index in range(3):
            created_folder = client.request(
                "POST", "/api/files/folders",
                payload={"parent_id": paging_id, "name": f"page-{index}"},
            )
            check(created_folder.status == 200, f"pagination child {index} creates")
        first_page = client.request(
            "GET", f"/api/files?folder_id={urllib.parse.quote(paging_id)}&offset=0&limit=2"
        )
        check(first_page.status == 200, "first private listing page succeeds")
        first_json = first_page.json()
        check(len(first_json.get("items", [])) == 2, "first private listing page is bounded to two items")
        next_offset = first_json.get("next_offset")
        check(next_offset == 2, f"first page advertises next offset (got {next_offset!r})")
        second_page = client.request(
            "GET", f"/api/files?folder_id={urllib.parse.quote(paging_id)}&offset=2&limit=2"
        )
        check(second_page.status == 200, "second private listing page succeeds")
        second_json = second_page.json()
        check(len(second_json.get("items", [])) == 1, "second page returns the remaining item")
        check(second_json.get("next_offset") is None, "last private page closes pagination")

        # A private file outside the backup namespace proves scope containment.
        private_name = "private-proof.txt"
        private_content = b"private outside backup scope"
        upload_body, upload_type = multipart(private_name, private_content)
        private_upload = client.request(
            "POST", "/api/files/upload", body=upload_body, content_type=upload_type
        )
        check(private_upload.status == 200, "full session uploads outside-backup proof file")
        private_node = private_upload.json().get("uploaded", [{}])[0]
        private_id = private_node.get("id")
        check(bool(private_id), "outside-backup node has an id")
        check(
            private_node.get("sha256") == hashlib.sha256(private_content).hexdigest(),
            "file listings include trusted SHA-256",
        )

        # Supported same-origin PDF iframe preview must not be blocked by the
        # global anti-clickjacking policy, while external framing remains denied.
        pdf_body, pdf_type = multipart("preview.pdf", b"%PDF-1.4\n% regression\n%%EOF\n")
        pdf_upload = client.request("POST", "/api/files/upload", body=pdf_body, content_type=pdf_type)
        check(pdf_upload.status == 200, "PDF preview fixture uploads")
        pdf_id = pdf_upload.json().get("uploaded", [{}])[0].get("id")
        pdf_content = client.request("GET", f"/api/files/{pdf_id}/content")
        xfo = pdf_content.headers.get("x-frame-options", "").upper()
        csp = pdf_content.headers.get("content-security-policy", "").lower()
        check(xfo == "SAMEORIGIN", f"same-origin PDF framing is allowed (XFO={xfo or 'missing'})")
        check("frame-ancestors 'self'" in csp, "CSP allows only same-origin preview framing")

        token_reply = client.request("POST", "/api/tokens", payload={"name": "regression backup", "scope": "backup"})
        check(token_reply.status == 200, f"backup-scoped token can be created (got {token_reply.status})")
        raw_token = token_reply.json().get("token")
        check(isinstance(raw_token, str) and raw_token.startswith("nmb_"), "backup token secret is returned once")
        backup = Client(base, raw_token)
        forbidden = backup.request("GET", "/api/settings/llm")
        check(forbidden.status == 403, "backup token cannot read provider settings")
        forbidden_tokens = backup.request("GET", "/api/tokens")
        check(forbidden_tokens.status == 403, "backup token cannot administer tokens")
        forbidden_docs = backup.request("GET", "/api/docs/tree")
        check(forbidden_docs.status == 403, "backup token cannot read the markdown vault")
        root_listing = backup.request("GET", "/api/files")
        check(root_listing.status == 200, "backup token can list its virtual root")
        items = root_listing.json().get("items", [])
        check(
            private_id not in {item.get("id") for item in items},
            "backup token root does not expose unrelated drive files",
        )
        backup_root = next((item for item in items if item.get("kind") == "folder" and item.get("name") == "Phone Backups"), None)
        check(backup_root is not None, "backup token virtual root exposes its Phone Backups folder")
        forbidden_node = backup.request("GET", f"/api/files/{private_id}")
        check(forbidden_node.status in (403, 404), "backup token cannot fetch an out-of-scope node")

        backup_root_id = backup_root.get("id")
        nested = backup.request(
            "POST", "/api/files/folders", payload={"parent_id": backup_root_id, "name": "regression-device"}
        )
        check(nested.status == 200, "backup token creates a folder inside its namespace")
        nested_id = nested.json().get("item", {}).get("id")
        old_content = b"old backup content"
        body, ctype = multipart("proof.txt", old_content)
        created_upload = backup.request(
            "POST", f"/api/files/upload?folder_id={urllib.parse.quote(nested_id)}",
            body=body, content_type=ctype,
        )
        check(created_upload.status == 200, "backup token uploads inside its namespace")
        created_node = created_upload.json().get("uploaded", [{}])[0]
        created_id = created_node.get("id")
        check(created_node.get("sha256") == hashlib.sha256(old_content).hexdigest(), "backup upload returns exact SHA")

        new_content = b"new backup content after same-length-ish edit"
        body, ctype = multipart("proof.txt", new_content)
        replaced = backup.request(
            "PUT", f"/api/files/{urllib.parse.quote(created_id)}/content",
            body=body, content_type=ctype,
        )
        check(replaced.status == 200, f"atomic file replacement succeeds (got {replaced.status})")
        replaced_node = replaced.json().get("item", {})
        check(replaced_node.get("id") == created_id, "replacement preserves node identity")
        check(
            replaced_node.get("sha256") == hashlib.sha256(new_content).hexdigest(),
            "replacement returns the new content SHA",
        )
        downloaded = backup.request("GET", f"/api/files/{created_id}/download")
        check(downloaded.body == new_content, "replacement content downloads byte-for-byte")

        out_of_scope_move = backup.request(
            "PATCH", f"/api/files/{created_id}", payload={"parent_id": "root"}
        )
        check(out_of_scope_move.status in (400, 403, 404), "backup token cannot move content outside scope")

        # Valid unsatisfiable single ranges must not silently return 200/full body.
        unsatisfiable = client.request(
            "GET", f"/api/files/{private_id}/content", headers={"Range": "bytes=999999999-"}
        )
        check(unsatisfiable.status == 416, f"unsatisfiable Range returns 416 (got {unsatisfiable.status})")

        # Public single-file payload omits private parent identifiers/timestamps.
        share = client.request(
            "POST", "/api/shares", payload={"target_type": "file", "target_id": private_id, "expires_at": None}
        )
        check(share.status == 200, "file share can be created")
        token = share.json().get("share", {}).get("token")
        public = Client(base).request("GET", f"/api/public/shares/{urllib.parse.quote(token)}")
        check(public.status == 200, "public share metadata is available")
        public_file = public.json().get("file", {})
        for private_field in ("parent_id", "updated_at", "created_at"):
            check(private_field not in public_file, f"public file payload omits {private_field}")

        if FAILURES:
            print(f"SECURITY_REGRESSIONS_FAILED ({len(FAILURES)} checks)", file=sys.stderr)
            for failure in FAILURES[:30]:
                print(f" - {failure}", file=sys.stderr)
            return 1
        print("SECURITY_REGRESSIONS_OK")
        return 0
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
        if log is not None:
            log.close()
        if args.keep_data:
            print(f"kept data at {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
