#!/usr/bin/env python3
"""Deterministic operational checks for the Nimbo repository.

This script validates deployment defaults, the production proxy example,
documentation claims, CI/release job coverage and obvious committed secrets or
private files. It uses only the Python standard library and does not require a
Docker daemon.

Exit 0 and print OPERATIONS_CHECKS_OK only when every check passes.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ERRORS: list[str] = []


def fail(message: str) -> None:
    ERRORS.append(message)


def read(relative: str) -> str:
    path = ROOT / relative
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        fail(f"missing required file: {relative}")
    except OSError as exc:
        fail(f"cannot read {relative}: {exc}")
    return ""


def require(relative: str) -> str:
    text = read(relative)
    if not text.strip():
        fail(f"required file is empty: {relative}")
    return text


def service_block(text: str, service: str) -> str:
    match = re.search(rf"^  {re.escape(service)}:\s*$", text, re.MULTILINE)
    if not match:
        fail(f"compose service not found: {service}")
        return ""
    start = match.end()
    following = re.search(r"^  [A-Za-z0-9_.-]+:\s*$", text[start:], re.MULTILINE)
    end = start + following.start() if following else len(text)
    return text[start:end]


def key_block(text: str, key: str, indent: int = 4) -> str:
    """Return the value block for a mapping key at the given indentation."""
    pattern = rf"^{' ' * indent}{re.escape(key)}:\s*$"
    match = re.search(pattern, text, re.MULTILINE)
    if not match:
        return ""
    start = match.end()
    following = re.search(
        rf"^{' ' * indent}[A-Za-z0-9_.-]+:\s*$", text[start:], re.MULTILINE
    )
    end = start + following.start() if following else len(text)
    return text[start:end]


def check_compose_default() -> None:
    text = require("docker/docker-compose.yml")
    block = service_block(text, "nimbo")
    if not block:
        return

    ports = key_block(block, "ports")
    if not ports:
        fail("docker-compose.yml: nimbo service has no ports block")
    if not re.search(r"\$\{NIMBO_PUBLISH_HOST:-127\.0\.0\.1\}", ports):
        fail("docker-compose.yml: published port does not default to 127.0.0.1")
    if re.search(r"0\.0\.0\.0:\$\{?NIMBO_PUBLISH_PORT", ports):
        fail("docker-compose.yml: published port defaults to all interfaces")
    if re.search(r"^\s*-\s*[\"']?8080:8080", ports, re.MULTILINE):
        fail("docker-compose.yml: publishes 8080 on all interfaces")

    expected = {
        "NIMBO_SECURE_COOKIES": "secure-cookie setting",
        "NIMBO_AUDIT_RETENTION_DAYS": "audit retention setting",
        "stop_grace_period: 30s": "graceful stop period",
        "healthcheck:": "container health check",
        "restart: unless-stopped": "restart policy",
        "read_only: true": "read-only root filesystem",
        "init: true": "init process",
        "no-new-privileges:true": "no-new-privileges",
        "nimbo_data:/data": "named data volume",
        "pids:": "pid limit",
        "env_file:": "optional environment file",
    }
    for needle, label in expected.items():
        if needle not in block:
            fail(f"docker-compose.yml: missing {label} ({needle})")

    if "cap_drop:" not in block or "ALL" not in block:
        fail("docker-compose.yml: missing cap_drop ALL")
    if "tmpfs:" not in block:
        fail("docker-compose.yml: read-only root needs a tmpfs")
    if "deploy:" not in block or "memory:" not in block or "cpus:" not in block:
        fail("docker-compose.yml: missing container resource limits")

    logging = key_block(block, "logging")
    if "max-size" not in logging or "max-file" not in logging:
        fail("docker-compose.yml: missing log rotation max-size/max-file")
    if "json-file" not in logging:
        fail("docker-compose.yml: log driver should be json-file for local rotation")

    health = key_block(block, "healthcheck")
    if "/healthz" not in health:
        fail("docker-compose.yml: health check does not probe /healthz")

    if "nimbo_nimbo_data" not in text:
        fail("docker-compose.yml: named volume should be deterministically named")
    if not re.search(r"^volumes:\s*$", text, re.MULTILINE):
        fail("docker-compose.yml: missing top-level volumes section")
    if ":latest" in text:
        fail("docker-compose.yml: must not use a :latest image")
    if 'NIMBO_SETUP_TOKEN: "${NIMBO_SETUP_TOKEN:-}"' in block:
        fail(
            "docker-compose.yml: must not set NIMBO_SETUP_TOKEN to an empty string; "
            "strict config rejects blank tokens"
        )


def check_compose_production() -> None:
    text = require("docker/docker-compose.production.yml")
    block = service_block(text, "nimbo")
    if not block:
        return

    if re.search(r"^    ports:\s*$", block, re.MULTILINE):
        fail("docker-compose.production.yml: nimbo must not publish a host port")
    if "expose:" not in block or "8080" not in block:
        fail("docker-compose.production.yml: nimbo should expose 8080 internally")
    if 'NIMBO_SECURE_COOKIES: "true"' not in block:
        fail("docker-compose.production.yml: secure cookies must be true")
    if 'NIMBO_SETUP_TOKEN: "${NIMBO_SETUP_TOKEN:?' not in block:
        fail("docker-compose.production.yml: setup token must be required")
    if 'image: "${NIMBO_IMAGE:?' not in block:
        fail("docker-compose.production.yml: image version must be required")
    if re.search(r"^    build:\s*$", block, re.MULTILINE):
        fail("docker-compose.production.yml: production must not build moving source")
    for needle, label in {
        "stop_grace_period: 30s": "graceful stop period",
        "read_only: true": "read-only root filesystem",
        "cap_drop:": "dropped capabilities",
        "no-new-privileges:true": "no-new-privileges",
        "NIMBO_AUDIT_RETENTION_DAYS": "audit retention setting",
        "tmpfs:": "tmpfs for a read-only root",
    }.items():
        if needle not in block:
            fail(f"docker-compose.production.yml: missing {label} ({needle})")

    proxy = service_block(text, "proxy")
    if not proxy:
        fail("docker-compose.production.yml: missing TLS proxy service")
        return
    for needle, label in {
        "nginx": "nginx proxy image",
        "443" : "HTTPS publication",
        "80": "HTTP redirect publication",
        "default.conf.template": "proxy template mount",
        "/etc/nginx/tls": "TLS certificate mount",
        "NIMBO_DOMAIN": "required public domain",
        "NIMBO_PROXY_BODY_LIMIT": "proxy body limit",
        "condition: service_healthy": "proxy waits for Nimbo health",
    }.items():
        if needle not in proxy:
            fail(f"docker-compose.production.yml: missing {label} ({needle})")

    if "read_only: true" not in proxy:
        fail("docker-compose.production.yml: proxy should use a read-only root")
    if 'user: "101:101"' not in proxy:
        fail("docker-compose.production.yml: proxy must run as unprivileged nginx UID/GID 101")
    if "uid=101" not in proxy or "gid=101" not in proxy:
        fail("docker-compose.production.yml: proxy tmpfs mounts must be owned by 101:101")
    if "cap_add:" in proxy:
        fail("docker-compose.production.yml: non-root proxy must not add capabilities")
    if "nginx -t" not in proxy:
        fail("docker-compose.production.yml: proxy health check must validate nginx config")
    if not re.search(r"@sha256:[0-9a-f]{64}", proxy):
        fail("docker-compose.production.yml: proxy image default must be digest-pinned")
    if ":latest" in proxy:
        fail("docker-compose.production.yml: proxy image must not use :latest")
    if "no-new-privileges:true" not in proxy:
        fail("docker-compose.production.yml: proxy needs no-new-privileges")
    if "logging:" not in proxy:
        fail("docker-compose.production.yml: proxy should rotate logs")
    if "nimbo_backend" not in text:
        fail("docker-compose.production.yml: missing isolated backend network")


def check_nginx() -> None:
    text = require("docker/nginx/default.conf.template")
    for needle, label in {
        "listen 8443 ssl": "TLS listener",
        "ssl_certificate": "TLS certificate",
        "ssl_certificate_key": "TLS key",
        "client_max_body_size ${NIMBO_PROXY_BODY_LIMIT}": "upload body limit",
        "proxy_set_header Host $http_host": "preserved Host header",
        "proxy_buffering off": "unbuffered SSE",
        "proxy_request_buffering off": "streamed uploads",
        "proxy_pass http://nimbo:8080": "backend upstream",
        "proxy_read_timeout": "long-lived stream timeout",
        "limit_req": "edge authentication throttling",
    }.items():
        if needle not in text:
            fail(f"nginx template: missing {label} ({needle})")
    if "return 301 https://$host$request_uri" not in text:
        fail("nginx template: port 80 must redirect to HTTPS")
    if "add_header Strict-Transport-Security" not in text:
        fail("nginx template: missing HSTS header")


def check_dockerfile() -> None:
    text = require("docker/Dockerfile")
    for needle, label in {
        "npm ci": "locked frontend install",
        "cargo build --release --locked": "locked release build",
        "cargo test --release --locked": "container test target",
        "COPY backend/vendor ./vendor": "vendored SQLite driver patch",
        "USER 10001:10001": "fixed non-root runtime user",
        "--uid 10001": "fixed runtime UID",
        "--gid 10001": "fixed runtime GID",
        "LABEL org.opencontainers.image": "OCI provenance labels",
        "ARG REVISION": "revision build argument",
        "ARG VERSION": "version build argument",
        "STOPSIGNAL SIGTERM": "graceful stop signal",
        "HEALTHCHECK": "image health check",
        'CMD curl -fsS http://127.0.0.1:8080/healthz': "working health-check command",
        "ca-certificates curl": "health-check client and CA bundle",
        'VOLUME ["/data"]': "declared data volume",
    }.items():
        if needle not in text:
            fail(f"Dockerfile: missing {label} ({needle})")
    for from_line in re.findall(r"^FROM\s+(\S+)", text, re.MULTILINE):
        if from_line.endswith(":latest"):
            fail(f"Dockerfile: mutable latest base image {from_line}")
        if from_line.startswith("${") or from_line == "backend":
            continue
        if "@sha256:" not in from_line:
            fail(f"Dockerfile: base image is not digest-pinned: {from_line}")
    for arg_name, arg_value in re.findall(
        r"^ARG\s+(NODE_IMAGE|RUST_IMAGE|RUNTIME_IMAGE)=(.+)$", text, re.MULTILINE
    ):
        value = arg_value.strip()
        if not re.search(r"@sha256:[0-9a-f]{64}$", value):
            fail(f"Dockerfile: {arg_name} default must be digest-pinned: {value}")
        if ":latest" in value:
            fail(f"Dockerfile: {arg_name} must not use :latest")


def check_env_example() -> None:
    text = require(".env.example")
    required = [
        "NIMBO_PUBLISH_HOST=127.0.0.1",
        "NIMBO_PUBLISH_PORT=8080",
        "# NIMBO_SETUP_TOKEN=",
        "NIMBO_SECURE_COOKIES=false",
        "NIMBO_SESSION_TTL_DAYS=30",
        "NIMBO_AUDIT_RETENTION_DAYS=90",
        "NIMBO_MAX_UPLOAD_MB=200",
        "NIMBO_CPUS=",
        "NIMBO_MEMORY=",
        "NIMBO_LOG=",
        "NIMBO_DOMAIN=",
        "NIMBO_TLS_DIR=",
        "chown 101:101",
        "NIMBO_PROXY_BODY_LIMIT=",
    ]
    for needle in required:
        if needle not in text:
            fail(f".env.example: missing {needle!r}")
    if not re.search(r"^# NIMBO_SETUP_TOKEN=\s*$", text, re.MULTILINE):
        fail(".env.example: NIMBO_SETUP_TOKEN must be a commented placeholder")
    if re.search(r"^NIMBO_SETUP_TOKEN=", text, re.MULTILINE):
        fail(".env.example: do not ship an active empty NIMBO_SETUP_TOKEN")
    if "openssl rand -hex 32" not in text:
        fail(".env.example: missing secret generation guidance")
    if "NIMBO_PROXY_IMAGE" in text and "@sha256:" not in text:
        fail(".env.example: proxy image example should show a digest pin")
    if ":latest" in text:
        fail(".env.example: do not encourage :latest tags")


def check_docs() -> None:
    readme = require("README.md")
    for needle in [
        "127.0.0.1",
        "NIMBO_SETUP_TOKEN",
        "docs/operations.md",
        "8 MiB",
        "16 MiB",
        "Phone Backups",
        "unmetered",
        "10,000 entries",
        "4 GiB",
        "8 GiB",
        "plaintext",
        "UID/GID 101",
        "nimbo.db-wal",
        "nimbo.db-shm",
        "secret.key",
    ]:
        if needle not in readme:
            fail(f"README.md: missing required claim {needle!r}")
    if "docker compose up --build -d" not in readme:
        fail("README.md: quick start is not the documented Compose command")

    operations = require("docs/operations.md")
    for heading in [
        "## Deployment model",
        "## First run and bootstrap",
        "## TLS and reverse proxy",
        "## Configuration reference",
        "## Backup",
        "## Restore",
        "## Restore drill",
        "## Upgrades and rollback",
        "## Retention, logs and maintenance",
        "## Troubleshooting and FAQ",
    ]:
        if heading not in operations:
            fail(f"docs/operations.md: missing section {heading!r}")
    for needle in [
        "docker compose stop nimbo",
        "nimbo.db-wal",
        "nimbo.db-shm",
        "secret.key",
        "10001",
        "PRAGMA integrity_check",
        "blob_sha",
        "thumb_sha",
        "RESTORE CHECK OK",
        "restore drill",
        "rollback",
        "VACUUM",
        "max-size",
        "proxy_buffering off",
        "proxy_set_header Host $http_host",
        "101:101",
        "chown 101:101",
        "chmod 700 tls",
        "chmod 600 tls/fullchain.pem tls/privkey.pem",
        "custom proxy",
        "Docker secrets",
        "fullchain.pem",
        "test -r /etc/nginx/tls/privkey.pem",
        "digest-pinned",
        "NIMBO_SETUP_TOKEN",
        "10,000 entries",
        "4 GiB",
        "8 GiB",
        "unmetered",
        "plaintext",
    ]:
        if needle not in operations:
            fail(f"docs/operations.md: missing operational detail {needle!r}")
    for forbidden in ["chmod 640 tls/", "chown -R 0:101 tls", "chmod 600 privkey.pem"]:
        if forbidden in operations:
            fail(
                "docs/operations.md: TLS ownership guidance conflicts with the "
                f"non-root proxy: {forbidden!r}"
            )

    architecture = require("docs/architecture.md")
    for needle in [
        "Provider API keys are encrypted at rest",
        "plaintext",
        "NIMBO_SETUP_TOKEN",
        "blocking task",
        "10,000 entries / 4 GiB",
        "8 GiB",
        "NIMBO_MAX_UPLOAD_MB",
        "NIMBO_AUDIT_RETENTION_DAYS",
    ]:
        if needle not in architecture:
            fail(f"docs/architecture.md: missing corrected claim {needle!r}")
    for forbidden in [
        "Everything sensitive is encrypted at rest",
        "size/time budgets",
        "time budgets",
    ]:
        if forbidden in architecture:
            fail(f"docs/architecture.md: stale claim remains: {forbidden!r}")

    security = require("SECURITY.md")
    for needle in [
        "NIMBO_SETUP_TOKEN",
        "127.0.0.1",
        "docs/operations.md",
        "plaintext",
        "backup-scoped",
        "UID/GID 101",
        "ANDROID_KEYSTORE_BASE64",
        "NIMBO_SECURE_COOKIES=true",
    ]:
        if needle not in security:
            fail(f"SECURITY.md: missing hardening detail {needle!r}")

    changelog = require("CHANGELOG.md")
    for needle in [
        "## [Unreleased]",
        "### Security",
        "### Fixed",
        "GHCR",
        "offline",
        "10001",
        "Backup-scoped",
    ]:
        if needle not in changelog:
            fail(f"CHANGELOG.md: missing release-note detail {needle!r}")

    contributing = require("CONTRIBUTING.md")
    for needle in [
        "scripts/verify-operations.py",
        "cargo test --locked",
        "cargo clippy --locked --all-targets",
        "npm run check",
        "node scripts/check-sanitizer.mjs",
        "testDebugUnitTest",
        "docker compose",
        "SECURITY.md",
    ]:
        if needle not in contributing:
            fail(f"CONTRIBUTING.md: missing check/release detail {needle!r}")

    placeholders = re.compile(r"\b(TODO|TBD|FIXME|XXX)\b")
    for relative in [
        "README.md",
        "SECURITY.md",
        "CHANGELOG.md",
        "CONTRIBUTING.md",
        "docs/architecture.md",
        "docs/operations.md",
    ]:
        text = read(relative)
        if placeholders.search(text):
            fail(f"{relative}: contains an unfinished placeholder marker")


def check_workflows() -> None:
    ci = require(".github/workflows/ci.yml")
    for job in ["backend:", "frontend:", "audit:", "docker:", "security-regressions:"]:
        if not re.search(rf"^  {re.escape(job)}", ci, re.MULTILINE):
            fail(f"ci.yml: missing job {job}")
    for needle, label in {
        "cargo test": "backend tests",
        "npm run check": "frontend type check",
        "npm run build": "frontend build",
        "node scripts/check-sanitizer.mjs": "frontend sanitizer controls",
        "npm audit --omit=dev": "production dependency audit",
        "cargo audit": "Rust dependency audit",
        "docker build": "Docker build",
        "-f docker/Dockerfile": "explicit Dockerfile path",
        "docker run": "disposable container run",
        "/healthz": "health smoke check",
        "config --quiet": "Compose validation",
        "scripts/verify-operations.py": "operational static checks",
        "docker volume": "persistence smoke check",
        "cargo build --manifest-path backend/Cargo.toml --release --locked": "release binary build",
        "python3 scripts/verify-security-regressions.py --binary backend/target/release/nimbo": "integrated security regression gate",
        "mkdir -p /tmp/opencode": "regression temp root",
    }.items():
        if needle not in ci:
            fail(f"ci.yml: missing {label} ({needle})")
    if "permissions:\n  contents: read" not in ci:
        fail("ci.yml: top-level permissions must be contents: read")
    if "packages: write" in ci:
        fail("ci.yml: PR CI must not have package-write permission")
    if "pull_request_target" in ci:
        fail("ci.yml: pull_request_target is forbidden")

    android = require(".github/workflows/android.yml")
    for needle, label in {
        "gradle/actions/wrapper-validation": "Gradle wrapper validation",
        "testDebugUnitTest": "Android unit tests",
        "lintDebug": "Android lint",
        "assembleDebug": "Android debug build",
    }.items():
        if needle not in android:
            fail(f"android.yml: missing {label} ({needle})")
    if "pull_request_target" in android:
        fail("android.yml: pull_request_target is forbidden")

    release = require(".github/workflows/release.yml")
    for needle, label in {
        'tags: ["v*.*.*"]': "tag-only release trigger",
        "packages: write": "GHCR package permission",
        "id-token: write": "OIDC provenance permission",
        "provenance: mode=max": "build provenance",
        "sbom: true": "SBOM generation",
        "push: true": "registry push",
        "ghcr.io/": "GHCR registry",
        "docker/login-action": "registry login",
        "startsWith(github.ref, 'refs/tags/v')": "tag guard",
    }.items():
        if needle not in release:
            fail(f"release.yml: missing {label} ({needle})")
    if "pull_request" in release:
        fail("release.yml: must not run on pull requests")

    android_release = require(".github/workflows/android-release.yml")
    for needle, label in {
        "ANDROID_KEYSTORE_BASE64": "keystore secret",
        "ANDROID_KEYSTORE_PASSWORD": "keystore password secret",
        "ANDROID_KEY_ALIAS": "key alias secret",
        "ANDROID_KEY_PASSWORD": "key password secret",
        "signing_available": "signing preflight output",
        "needs.preflight.outputs.signing_available == 'true'": "fail-closed release gate",
        "assembleRelease": "release build",
        "apksigner": "signature verification",
        "versionName": "tag/version validation",
        "No APK was produced": "explicit skip message",
    }.items():
        if needle not in android_release:
            fail(f"android-release.yml: missing {label} ({needle})")
    if 'getByName("debug")' in android_release:
        fail("android-release.yml: must not fall back to debug signing")

    wrapper = require("android/gradle/wrapper/gradle-wrapper.properties")
    if not re.search(r"^distributionSha256Sum=[0-9a-f]{64}$", wrapper, re.MULTILINE):
        fail("Gradle wrapper: distributionSha256Sum must be a verified SHA-256")
    if "validateDistributionUrl=true" not in wrapper:
        fail("Gradle wrapper: validateDistributionUrl must be enabled")

    for relative in [
        ".github/workflows/ci.yml",
        ".github/workflows/android.yml",
        ".github/workflows/release.yml",
        ".github/workflows/android-release.yml",
    ]:
        text = read(relative)
        if "pull_request_target" in text:
            fail(f"{relative}: pull_request_target is forbidden")
        for line in text.splitlines():
            match = re.match(r"^\s*(?:-\s*)?uses:\s*([^\s#]+)(.*)$", line)
            if not match:
                continue
            action, trailing = match.groups()
            if action.startswith("./") or action.startswith("docker://"):
                continue
            if not re.search(r"@[0-9a-f]{40}$", action):
                fail(f"{relative}: action reference is not a full commit SHA: {action}")
            if "#" not in trailing:
                fail(f"{relative}: pinned action is missing a version comment: {action}")


def tracked_files() -> list[str]:
    try:
        result = subprocess.run(
            ["git", "ls-files", "-z"],
            cwd=ROOT,
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        fail(f"cannot list tracked files: {exc}")
        return []
    return [item for item in result.stdout.decode("utf-8", "replace").split("\0") if item]


def check_secrets_and_private_files() -> None:
    files = tracked_files()
    private_path = re.compile(
        r"(^|/)(\.env|secret\.key|id_rsa|id_ed25519|nimbo\.db(-wal|-shm)?)$"
        r"|\.(jks|keystore|p12|pfx|pem|key)$"
    )
    for relative in files:
        if relative == ".env.example":
            continue
        if private_path.search(relative):
            fail(f"tracked private/credential file: {relative}")

    secret_patterns = {
        "PEM private key": re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
        "AWS access key": re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
        "GitHub token": re.compile(r"\b(?:ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{20,})\b"),
        "Slack token": re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
        "Nimbo device token": re.compile(r"\bnmb_[0-9a-fA-F]{32,}\b"),
    }
    for relative in files:
        path = ROOT / relative
        if not path.is_file():
            continue
        try:
            if path.stat().st_size > 2 * 1024 * 1024:
                continue
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for label, pattern in secret_patterns.items():
            if pattern.search(text):
                fail(f"possible committed secret ({label}) in {relative}")

    dockerignore = require(".dockerignore")
    for needle in [
        ".unlazy",
        "GATES.md",
        ".github",
        "android",
        "docs",
        "scripts",
        "**/tests",
        "**/*.test.*",
        "*.db",
        "*.key",
        "*.keystore",
        "*.jks",
        ".env",
        "frontend/build",
        "frontend/node_modules",
    ]:
        if needle not in dockerignore:
            fail(f".dockerignore: missing exclusion {needle!r}")
    for protected in [
        "frontend/src",
        "backend/src",
        "backend/migrations",
        "frontend/package-lock.json",
        "backend/Cargo.lock",
    ]:
        if re.search(rf"^{re.escape(protected)}\s*$", dockerignore, re.MULTILINE):
            fail(f".dockerignore: excludes required build input {protected!r}")


def main() -> int:
    required = [
        "docker/Dockerfile",
        "docker/docker-compose.yml",
        "docker/docker-compose.production.yml",
        "docker/nginx/default.conf.template",
        "docker/tls/.gitignore",
        ".dockerignore",
        ".env.example",
        "README.md",
        "SECURITY.md",
        "CHANGELOG.md",
        "CONTRIBUTING.md",
        "docs/architecture.md",
        "docs/operations.md",
        "scripts/verify-operations.py",
        "scripts/verify-security-regressions.py",
        ".github/workflows/ci.yml",
        ".github/workflows/android.yml",
        ".github/workflows/release.yml",
        ".github/workflows/android-release.yml",
    ]
    for relative in required:
        if not (ROOT / relative).is_file():
            fail(f"missing required file: {relative}")

    if (ROOT / "docker/docker-compose.yml").is_file():
        check_compose_default()
    if (ROOT / "docker/docker-compose.production.yml").is_file():
        check_compose_production()
    if (ROOT / "docker/nginx/default.conf.template").is_file():
        check_nginx()
    if (ROOT / "docker/Dockerfile").is_file():
        check_dockerfile()
    if (ROOT / ".env.example").is_file():
        check_env_example()
    check_docs()
    check_workflows()
    check_secrets_and_private_files()

    if ERRORS:
        print("OPERATIONS_CHECKS_FAILED", file=sys.stderr)
        for error in ERRORS:
            print(f"- {error}", file=sys.stderr)
        return 1

    print("OPERATIONS_CHECKS_OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
