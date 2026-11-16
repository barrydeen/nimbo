# Operations runbook

This is the operator-facing procedure for deploying, backing up, restoring and
upgrading a self-hosted Nimbo instance. It describes the shipping single-host
Docker deployment. Commands assume the repository is checked out at
`/srv/nimbo`; adjust paths to your host.

Nimbo stores everything in one named volume. There is no supported
multi-replica mode, external database, or online backup service. A verified
offline backup is a prerequisite for every upgrade.

## Deployment model

- One `nimbo` container, one data volume, one SQLite database in WAL mode.
- The application listens on `0.0.0.0:8080` **inside** the container.
- The development Compose file publishes that port on `127.0.0.1` by default.
  Only a local browser or an SSH tunnel should reach it.
- The production example puts an nginx TLS proxy on the edge and publishes no
  Nimbo port at all.
- Supported architecture: a single host with a local POSIX filesystem (or a
  filesystem that fully preserves POSIX locking and atomic rename semantics).
  Do not put `/data` on object storage, NFS/SMB, or a filesystem shared by
  multiple writers.

Resource expectations for a small personal instance: 2 vCPU, 2 GiB RAM,
10 GiB free disk plus the size of your files. Image extraction and the vision
worker can be memory hungry; the container limits are a backstop, not a
guarantee that a hostile parser will be killed quickly.

## First run and bootstrap

A fresh instance has `setup_complete=false`, and `POST /api/setup` creates the
first administrator. Whoever can reach that endpoint first can claim the
instance, so the default deployment is loopback-only.

1. Create the environment file:

   ```bash
   cd /srv/nimbo/docker
   cp ../.env.example .env
   chmod 600 .env
   ```

2. Generate a one-time setup token and put it in `.env`:

   ```bash
   token="$(openssl rand -hex 32)"
   printf 'NIMBO_SETUP_TOKEN=%s\n' "$token" >> .env
   printf 'Setup token: %s\n' "$token"
   ```

   Keep the value somewhere safe until setup is complete. When
   `NIMBO_SETUP_TOKEN` is set, the setup request must include it; the setup page
   asks for it.

3. Start the local instance:

   ```bash
   docker compose up --build -d
   docker compose ps
   docker compose logs -f nimbo
   ```

4. Open <http://127.0.0.1:8080>, paste the token, and create the administrator.

5. Confirm `/api/bootstrap` reports `"setup_complete": true` and that the login
   succeeds.

For a remote host, do **not** expose this plain-HTTP port. Either use an SSH
tunnel (`ssh -L 8080:127.0.0.1:8080 user@host`) for first-run setup, or finish
the TLS sequence below first and complete setup through HTTPS. TLS must be live
before remote credentials or a device token cross the network.

After setup, the token is no longer needed. In the development Compose file,
removing `NIMBO_SETUP_TOKEN` and recreating the container is safe; a completed
instance rejects setup regardless. The production example intentionally requires
the variable so a fresh production instance can never start without a bootstrap
secret: keep the same value (or any new random value) in `.env` after setup. It
is ignored once `setup_complete=true`.

## TLS and reverse proxy

Use `docker-compose.production.yml` as the starting point. It changes three
things that must stay true together:

1. Nimbo publishes no host port, so the proxy is the only path to it.
2. `NIMBO_SECURE_COOKIES=true`, so browsers only send the session cookie over
   HTTPS.
3. The proxy preserves the browser `Host` header and disables response
   buffering.

The proxy itself runs as the image's unprivileged `101:101` user with a
read-only root filesystem; its writable paths are tmpfs mounts owned by
`101:101`. It binds unprivileged ports 8080/8443 inside the container, so no
privileged-port capability is required. The official image may log that its
`user` directive is ignored when the master is already non-root; that warning
is expected. If you pin a proxy image with a different unprivileged UID, update
`user` and the tmpfs `uid=`/`gid=` options together. The example listens on IPv4;
add `listen [::]` lines only when the container actually has IPv6 enabled.

### Certificates

Place a certificate chain and key in `NIMBO_TLS_DIR` (default `docker/tls`):

```text
docker/tls/fullchain.pem
docker/tls/privkey.pem
```

The proxy container runs as the official nginx image's unprivileged
`101:101` user, so the certificate directory and files must be owned by that
UID/GID (or otherwise readable by it). Root-owned mode-600 certificates are not
readable by the proxy and will make nginx config/start fail. From the `docker/`
directory:

```bash
sudo chown 101:101 tls tls/fullchain.pem tls/privkey.pem
sudo chmod 700 tls
sudo chmod 600 tls/fullchain.pem tls/privkey.pem
```

The proxy needs traverse access to the directory and read access to both files
as UID 101. The start sequence below includes a pre-start readability check.

If you use a custom proxy image, or change its `user:`/tmpfs `uid=`/`gid=`
values, give the certificate files the matching owner and group. If you inject
certificates through Docker secrets instead of a bind mount, set the secret's
`uid`, `gid` and `mode` so UID 101 can read it; the default root-owned secret
mode is not readable by this non-root proxy.

Reapply ownership/modes after renewal, then reload the proxy (see the production
start sequence). Obtain certificates from your ACME client or certificate
authority; Nimbo does not manage them. Never commit this directory.

### Start the production example

```bash
cd /srv/nimbo/docker
cp ../.env.example .env
chmod 600 .env
# Edit .env and set NIMBO_IMAGE and NIMBO_DOMAIN first.
token="$(openssl rand -hex 32)"
printf 'NIMBO_SETUP_TOKEN=%s\n' "$token" >> .env
printf 'Setup token: %s\n' "$token"
docker compose -f docker-compose.production.yml pull
# Verify the non-root proxy can read the TLS material before starting it.
docker compose -f docker-compose.production.yml run --rm --no-deps \
  --entrypoint sh proxy -c \
  'test -r /etc/nginx/tls/fullchain.pem && test -r /etc/nginx/tls/privkey.pem'
docker compose -f docker-compose.production.yml config --quiet
docker compose -f docker-compose.production.yml up -d
docker compose -f docker-compose.production.yml ps
docker compose -f docker-compose.production.yml exec proxy nginx -t
curl -fsS "https://nimbo.example.com/healthz"
```

Replace the example hostname with your `NIMBO_DOMAIN`. `NIMBO_IMAGE` must be a
published version or digest. Do not deploy `latest`: rollback and migration
compatibility depend on knowing the exact image.

### Header and origin behavior

Nimbo rejects mutating requests (`POST`, `PUT`, `PATCH`, `DELETE`) when the
`Origin` header's host differs from the request `Host`. The supplied template
uses `proxy_set_header Host $http_host;` for this reason. Do not rewrite `Host`
to `nimbo:8080`; browser saves and uploads will fail with 403.

The external URL, the proxy `server_name`, and the hostname in the browser
address bar must agree. If you use a non-default HTTPS port, `$http_host`
preserves it on proxied requests, but the plain-HTTP redirect still targets the
standard HTTPS port; update the redirect in the template for a custom external
port.

### Upload and SSE behavior

- `client_max_body_size` must be at least Nimbo's whole-request cap. Defaults:
  200 MiB per file plus multipart overhead, so `201m`. Adjust
  `NIMBO_PROXY_BODY_LIMIT` when you change `NIMBO_MAX_UPLOAD_MB`.
- `proxy_request_buffering off` streams uploads instead of staging the whole
  request on the proxy.
- `proxy_buffering off` is required for agent chat SSE and is also suitable for
  downloads.
- The template throttles `/api/auth/login` and `/api/setup` at the edge in
  addition to the application limiter.

### Bypass prevention

- Do not add a `ports:` entry to the `nimbo` service in production.
- Block container-port access from untrusted networks at the host firewall.
- Keep the proxy certificate valid; a expired certificate is an outage, not a
  reason to fall back to HTTP.

## Configuration reference

Compose reads `docker/.env`. The application also accepts the same variables
when run directly.

| Variable | Default | Meaning |
| --- | --- | --- |
| `NIMBO_PUBLISH_HOST` | `127.0.0.1` | Host interface for the development Compose port |
| `NIMBO_PUBLISH_PORT` | `8080` | Host port for the development Compose port |
| `NIMBO_BIND` | `127.0.0.1:8080` | Listen address; Compose sets `0.0.0.0:8080` in the container |
| `NIMBO_DATA_DIR` | `/data` in the image | Database, key, vault and blob root |
| `NIMBO_STATIC_DIR` | `/app/static` in the image | Built frontend |
| `NIMBO_SETUP_TOKEN` | unset | One-time bootstrap secret for `POST /api/setup` (16–512 bytes; omit when disabled) |
| `NIMBO_SECURE_COOKIES` | `false` | Add `Secure` to the session cookie; requires HTTPS |
| `NIMBO_SESSION_TTL_DAYS` | `30` | Session lifetime in days (1–3650) |
| `NIMBO_MAX_UPLOAD_MB` | `200` | Per-file upload ceiling and request-limit basis (1–10240 MiB) |
| `NIMBO_AUDIT_RETENTION_DAYS` | `90` | Audit-row retention in days (1–3650) |
| `NIMBO_CPUS` / `NIMBO_MEMORY` | `2.0` / `2g` | Container CPU/memory limits |
| `NIMBO_LOG` | `nimbo=info,tower_http=info` | `RUST_LOG` filter |
| `NIMBO_IMAGE` | `nimbo:local` (build) | Image used by Compose |
| `NIMBO_DOMAIN` | required in production | Public DNS name for the proxy |
| `NIMBO_TLS_DIR` | `./tls` | Directory with `fullchain.pem` and `privkey.pem` |
| `NIMBO_EDGE_BIND` | `0.0.0.0` | Proxy bind address for ports 80/443 |
| `NIMBO_HTTP_PORT` / `NIMBO_HTTPS_PORT` | `80` / `443` | Published proxy ports |
| `NIMBO_PROXY_BODY_LIMIT` | `201m` | Nginx request-body ceiling |
| `NIMBO_PROXY_IMAGE` | digest-pinned `nginx:1.28-alpine` | Proxy image override |
| `NIMBO_PROXY_CPUS` / `NIMBO_PROXY_MEMORY` | `1.0` / `256m` | Proxy resource limits |

Invalid or out-of-range values must fail startup rather than silently falling
back. Check `docker compose logs nimbo` after changing configuration.

### Effective request limits

| Operation | Limit | Notes |
| --- | --- | --- |
| Note content | 8 MiB | Application-level content check |
| Note save JSON body | 16 MiB | Route-specific Axum body limit, enough for escaping/overhead |
| One uploaded file | `NIMBO_MAX_UPLOAD_MB` (200 MiB default) | Multipart `file` field |
| One upload request | about per-file limit + 1 MiB | Aggregate multipart cap; batch smaller |
| Files per upload request | 100 | Count check |
| Public folder archive | 10,000 entries and 4 GiB of source bytes | Per archive, staged below `/data/files/tmp` |
| Concurrent public archives | 2 builds and 8 GiB aggregate staging | Global bounds; excess requests are rejected |
| Agent/provider request | provider-specific timeout and byte limits | Container limits are not an LLM timeout |

A document near 8 MiB with heavy JSON escaping can hit the 16 MiB body limit
first. A batch whose files are individually valid can still exceed the aggregate
multipart cap. Both are deliberate; split the content or upload.

## Backup

Back up the **whole volume while Nimbo is stopped**. Do not copy
`nimbo.db` alone, and do not use a live recursive `cp`, `rsync` or `docker cp`
of selected paths. SQLite WAL ordering plus concurrent vault/blob writes can
produce a database that references files the copy never captured.

The volume contains:

```text
/data/nimbo.db          SQLite database
/data/nimbo.db-wal      write-ahead log (may be present)
/data/nimbo.db-shm      shared-memory index (may be present)
/data/secret.key        32-byte instance key for provider credentials
/data/vault/            plaintext markdown notes
/data/files/blobs/      content-addressed uploaded bytes and thumbnails
/data/files/tmp/        transient upload/zip staging (safe to omit)
```

The Compose files name the volume `nimbo_nimbo_data`. If you upgraded from an
older deployment with a different volume name, substitute it in the commands
below (find it with `docker volume ls | grep nimbo`).

`secret.key` is mandatory: without it, stored provider API keys cannot be
decrypted. Notes, database rows, blobs and backups are **not** encrypted by the
application; protect the host and the backup destination with filesystem
encryption and access controls.

### Offline backup (recommended)

Run from `docker/` on the host. Use the same Compose file you deployed with;
the example below uses the development file. For the production example,
substitute `-f docker-compose.production.yml` in every `docker compose`
command.

```bash
cd /srv/nimbo/docker
docker compose stop nimbo
docker volume inspect nimbo_nimbo_data >/dev/null
mkdir -p backups
chmod 700 backups

docker run --rm \
  -v nimbo_nimbo_data:/data:ro \
  -v "$PWD/backups:/backup" \
  alpine:3 \
  sh -c 'cd /data && tar -czf "/backup/nimbo-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" .'

docker compose start nimbo
archive="$(ls -1t backups/nimbo-*.tar.gz | head -n 1)"
sha256sum "$archive" | tee "${archive}.sha256"
chmod 600 "$archive" "${archive}.sha256"
```

Record the image reference and digest with the archive:

```bash
docker inspect --format '{{.Config.Image}}' nimbo
docker image inspect --format '{{join .RepoDigests "\n"}}' ghcr.io/barrydeen/nimbo:0.2.0
```

Store at least one copy off the host. A backup that has never been restored is
an untested assumption, not a recovery plan.

### Online-consistent backup

If you cannot stop the container, take an atomic filesystem or block-device
snapshot of the entire data volume and the database/WAL/key together. SQLite's
`.backup` command alone is not enough because it does not capture the vault or
blob files at the same point in time. Restore-test any snapshot procedure
before relying on it.

### What not to do

- Do not archive only the paths you remember; archive the volume root.
- Do not omit `nimbo.db-wal`, `nimbo.db-shm` or `secret.key`.
- Do not copy a running database with `cp` and assume the blobs match.
- Do not store plaintext backups on a machine you would not trust with the
  notes themselves.

## Restore

Restore into a **fresh volume**, verify it, and only then cut over. Never
restore over the only copy of a volume or database.

### 1. Stop the stack

```bash
cd /srv/nimbo/docker
docker compose -f docker-compose.production.yml down
```

`down` removes containers and the network but keeps the named volume.

### 2. Create and unpack a fresh volume

```bash
# Remove or rename an old drill volume only after confirming it is disposable.
docker volume create nimbo_restore

docker run --rm \
  -v nimbo_restore:/data \
  -v "$PWD/backups:/backup:ro" \
  alpine:3 \
  sh -c 'cd /data && tar -xzf /backup/nimbo-YYYYMMDDTHHMMSSZ.tar.gz'

# The image runs as UID/GID 10001. Fix ownership after extracting as root.
docker run --rm -v nimbo_restore:/data alpine:3 sh -c \
  'chown -R 10001:10001 /data \
   && find /data -type d -exec chmod 700 {} \; \
   && find /data -type f -exec chmod 600 {} \;'
```

Keep the archive's `secret.key`. Replacing it makes existing provider
credentials undecryptable.

### 3. Check database and blob integrity

This check opens the restored database read-write while no Nimbo process is
running, validates `PRAGMA integrity_check`, verifies that every blob row has a
readable file with the recorded size and SHA-256, and verifies that file nodes
reference existing blob rows and files. It also checks `secret.key` length and
ownership.

```bash
docker run --rm -i --user 10001:10001 -v nimbo_restore:/data \
  python:3.13-slim python3 - <<'PY'
import hashlib
import pathlib
import sqlite3
import sys

root = pathlib.Path("/data")
errors = []

db_path = root / "nimbo.db"
if not db_path.is_file():
    errors.append("missing nimbo.db")
    print("\n".join(errors))
    sys.exit(1)

secret = root / "secret.key"
if not secret.is_file():
    errors.append("missing secret.key")
elif secret.stat().st_size != 32:
    errors.append(f"secret.key is {secret.stat().st_size} bytes, expected 32")
elif secret.stat().st_uid != 10001:
    errors.append(f"secret.key uid is {secret.stat().st_uid}, expected 10001")

if not (root / "vault").is_dir():
    errors.append("missing vault directory")
if not (root / "files" / "blobs").is_dir():
    errors.append("missing files/blobs directory")

db = sqlite3.connect(db_path)
status = db.execute("PRAGMA integrity_check").fetchone()[0]
if status != "ok":
    errors.append(f"PRAGMA integrity_check: {status}")

foreign_key_errors = db.execute("PRAGMA foreign_key_check").fetchall()
for row in foreign_key_errors:
    errors.append(f"foreign key violation: {row}")

blobs = db.execute("SELECT sha256, size FROM blobs").fetchall()
checked = 0
for sha, size in blobs:
    path = root / "files" / "blobs" / sha[:2] / sha[2:4] / sha
    if not path.is_file():
        errors.append(f"missing blob file {sha}")
        continue
    if path.stat().st_size != size:
        errors.append(f"blob size mismatch {sha}")
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != sha:
        errors.append(f"blob digest mismatch {sha}")
    checked += 1

missing = db.execute(
    """
    SELECT n.id, n.name, n.blob_sha
    FROM nodes n
    LEFT JOIN blobs b ON b.sha256 = n.blob_sha
    WHERE n.kind = 'file' AND n.blob_sha IS NOT NULL AND b.sha256 IS NULL
    """
).fetchall()
for node_id, name, sha in missing:
    errors.append(f"file node {node_id} ({name}) references missing blob {sha}")

missing_thumbs = db.execute(
    """
    SELECT n.id, n.name, n.thumb_sha
    FROM nodes n
    LEFT JOIN blobs b ON b.sha256 = n.thumb_sha
    WHERE n.thumb_sha IS NOT NULL AND b.sha256 IS NULL
    """
).fetchall()
for node_id, name, sha in missing_thumbs:
    errors.append(f"node {node_id} ({name}) references missing thumbnail {sha}")

nodes = db.execute(
    "SELECT COUNT(*) FROM nodes WHERE kind = 'file' AND blob_sha IS NOT NULL"
).fetchone()[0]
db.close()

if errors:
    print("RESTORE CHECK FAILED")
    for error in errors:
        print(f"- {error}")
    sys.exit(1)

print(f"RESTORE CHECK OK: {nodes} file nodes, {checked} blob objects verified")
PY
```

If this exits non-zero, do not start the instance. Restore a different archive.

### 4. Run a restore drill before cutover

Start the restored volume on a loopback-only port with the same image you
intend to run. Do not point the proxy at it yet.

```bash
# Use the exact image tag or digest you intend to deploy.
image="ghcr.io/barrydeen/nimbo:0.2.0"
docker run -d --name nimbo-restore \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --security-opt no-new-privileges:true --cap-drop ALL \
  -p 127.0.0.1:18080:8080 \
  -v nimbo_restore:/data \
  "$image"

# Wait for the health check, then inspect the result.
curl -fsS http://127.0.0.1:18080/healthz
curl -fsS http://127.0.0.1:18080/api/bootstrap
docker logs nimbo-restore
```

Open the UI on <http://127.0.0.1:18080>, sign in with an existing account, open a
known note, download a known file, and open a share link. Then stop and remove
the drill container:

```bash
docker rm -f nimbo-restore
```

### 5. Cut over

Once verified, restore the same archive into the canonical volume name used by
Compose. Preserve the old volume first if it still exists, and do not remove it
until the copy has completed successfully and the restored instance passes its
checks.

```bash
docker run --rm \
  -v nimbo_nimbo_data:/from:ro \
  -v nimbo_nimbo_data_previous:/to \
  alpine:3 \
  sh -c 'cd /from && tar -cf - . | (cd /to && tar -xf -)'

docker volume rm nimbo_nimbo_data
docker volume create nimbo_nimbo_data

docker run --rm \
  -v nimbo_nimbo_data:/data \
  -v "$PWD/backups:/backup:ro" \
  alpine:3 \
  sh -c 'cd /data && tar -xzf /backup/nimbo-YYYYMMDDTHHMMSSZ.tar.gz'

docker run --rm -v nimbo_nimbo_data:/data alpine:3 sh -c \
  'chown -R 10001:10001 /data \
   && find /data -type d -exec chmod 700 {} \; \
   && find /data -type f -exec chmod 600 {} \;'

docker compose -f docker-compose.production.yml up -d
```

The `nimbo_nimbo_data_previous` volume can be removed after the restored
instance has passed its checks.

## Restore drill

Perform a drill after the first deployment and before every upgrade that
changes schema:

1. Take a fresh offline backup with the instance stopped.
2. Restore it into `nimbo_restore` as above.
3. Run the integrity check.
4. Start the restored volume on loopback with the **target** image.
5. Sign in, read a document, download a blob, open a share, and verify file
   counts in the UI.
6. Record the date, archive checksum, image digest, and result.
7. Remove the drill container and volume (`docker rm -f nimbo-restore` and
   `docker volume rm nimbo_restore`).

A drill that only checks `/healthz` does not prove that notes, blobs, shares,
credentials, or the admin account survived. Keep the last successful archive
and its checksum off-host.

## Upgrades and rollback

Nimbo applies embedded migrations automatically at startup. Migrations are
forward-only. An older image must not run against a database migrated by a newer
image.

### Upgrade procedure

1. Read the release notes for schema, configuration and API changes.
2. Take and verify an offline backup (see above). Record the current image
   digest.
3. Pull the exact target version:

   ```bash
   cd /srv/nimbo/docker
   NIMBO_IMAGE=ghcr.io/barrydeen/nimbo:0.2.0 docker compose \
     -f docker-compose.production.yml pull
   ```

4. Update `NIMBO_IMAGE` in `.env` to that exact version or digest.
5. Recreate the service and watch startup:

   ```bash
   docker compose -f docker-compose.production.yml up -d
   docker compose -f docker-compose.production.yml logs -f nimbo
   ```

6. Wait for health, then run the post-upgrade checks: sign in, read/write a
   note, upload/download a file, search, open a share, and confirm the Android
   backup token still syncs.
7. Keep the previous image and the pre-upgrade archive until the new version
   has passed the restore drill.

### Rollback procedure

1. Stop the service.
2. Restore the pre-upgrade archive into the canonical volume (or point Compose
   at the preserved pre-upgrade volume).
3. Set `NIMBO_IMAGE` back to the recorded previous digest.
4. Start the service and repeat the post-upgrade checks.

Do not "roll back" by only changing the image tag while leaving the newer
database in place. If the newer image migrated the schema, the old image may
misread it or fail.

### Version requirements

- Production deployments must pin an exact release tag (`X.Y.Z`) or image
  digest. `latest` and local `nimbo:local` builds are for development.
- Images are published to GHCR only when a `v*.*.*` tag is pushed. Each
  published image carries OCI source/revision/version labels and provenance.
- The Dockerfile's Node, Rust and Debian bases are pinned by multi-arch digest;
  the production proxy default is digest-pinned as well. CI action references
  are pinned to reviewed full commit SHAs with version comments, and the
  Gradle wrapper validates a pinned distribution checksum.
- For a source build, run `docker compose build --pull` to refresh any
  tag-based overrides, then record the resulting local image ID/digest.
  Digest-pinned bases only change when the pin is reviewed and updated. A
  source build is not a substitute for a published release artifact when you
  need rollback.
- CI builds the release binary and runs
  `scripts/verify-security-regressions.py` against a disposable instance before
  release publication. Treat a failure there as a release blocker.
- Keep the prior image locally until the new version is accepted.

## Retention, logs and maintenance

### Container logs

Both Compose files rotate the Docker `json-file` log with `max-size: 10m` and
`max-file: 3`, capping each service at roughly 30 MiB. If your daemon has a
global log policy, confirm it does not conflict. To inspect logs:

```bash
docker compose -f docker-compose.production.yml logs --tail 200 nimbo
docker compose -f docker-compose.production.yml logs --tail 200 proxy
```

Logs contain request metadata and errors. Treat them as potentially sensitive
operational data: do not publish them, and do not enable verbose request-body
logging in a proxy.

### Database growth

The application purges expired sessions and audit rows older than
`NIMBO_AUDIT_RETENTION_DAYS` at startup and periodically while it runs. Choose
a retention window commensurate with your security-review needs; the default is
90 days. Device-token `last_used_at` writes are throttled, but a long-running
instance still benefits from periodic checkpointing and compaction.

Run maintenance only while the instance is stopped, and only after a verified
backup:

```bash
cd /srv/nimbo/docker
docker compose -f docker-compose.production.yml stop nimbo

docker run --rm -i --user 10001:10001 -v nimbo_nimbo_data:/data python:3.13-slim python3 - <<'PY'
import sqlite3

db = sqlite3.connect("/data/nimbo.db")
db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
db.execute("PRAGMA optimize")
# Optional extra cleanup. The application already applies session expiry and
# NIMBO_AUDIT_RETENTION_DAYS; review any custom cutoff before running it.
# db.execute("DELETE FROM sessions WHERE expires_at < strftime('%s','now')")
# db.execute("DELETE FROM audit_log WHERE at < strftime('%s','now') - 90*86400")
db.commit()
db.execute("VACUUM")
db.close()
print("maintenance OK")
PY

docker compose -f docker-compose.production.yml start nimbo
```

`VACUUM` needs free space roughly equal to the database size and temporarily
blocks writers. Run it during a maintenance window.

### Disk and temp files

Uploads and public folder archives stage files below `/data/files/tmp`. Normal
request completion and startup reconciliation remove stale staging files. If
the volume fills, uploads, SQLite writes and downloads can all fail:

```bash
docker system df
docker run --rm -v nimbo_nimbo_data:/data alpine:3 \
  sh -c 'du -sh /data/* /data/files/* 2>/dev/null'
```

Do not delete `nimbo.db`, `secret.key`, `vault/` or `files/blobs/` by hand.
Remove only clearly named stale staging files after stopping the service and
taking a backup.

## Troubleshooting and FAQ

**The UI says the request is cross-origin (403) behind a proxy.**
The proxy is rewriting `Host`. Preserve the browser value with
`proxy_set_header Host $http_host;` and make the external URL match the
configured hostname.

**Login appears to succeed but immediately returns to the login page.**
`NIMBO_SECURE_COOKIES=true` is set while the browser is using plain HTTP, or
the proxy terminates TLS but the browser is hitting an HTTP URL. Use HTTPS for
every request, or set secure cookies to false only for a strictly local HTTP
trial.

**A note save fails around 8–16 MiB.**
The decoded note content is capped at 8 MiB and the JSON body at 16 MiB. JSON
escaping counts toward the body limit. Split very large notes.

**An upload fails with 413 even though each file is under the limit.**
The whole multipart request is allowed only about `NIMBO_MAX_UPLOAD_MB + 1 MiB`,
and at most 100 files are accepted per request. Upload fewer/smaller files, or
raise `NIMBO_MAX_UPLOAD_MB` and `NIMBO_PROXY_BODY_LIMIT` together.

**Agent chat streams slowly or stops updating.**
Disable proxy buffering (`proxy_buffering off`) and use HTTP/1.1 or HTTP/2
without response compression at the proxy. Nimbo streams SSE; a buffering
proxy hides deltas until the response ends.

**A public folder ZIP takes a long time or fails.**
Archives are built synchronously before the response starts. Each archive is
capped at 10,000 entries and 4 GiB of source bytes; at most two build at once
with an 8 GiB aggregate staging budget, and staging consumes space on the same
volume as the database. Share smaller folders, restrict public sharing, and keep
free space headroom.

**The health check is green but indexing seems stuck.**
`/healthz` is a bounded readiness check for the database and required data
paths; it does not prove that every background indexing job succeeds. Use
`/livez` for process liveness. Check logs, retry extraction from the file
details view, and confirm the container has enough memory. A poison file is
retried a bounded number of times and then marked failed; remove or replace it
after a backup before retrying.

**How do I reset an administrator password?**
There is no supported password-reset CLI. Restore a backup that contains a
known credential, or recover the account through a database backup taken before
the password was lost. Do not hand-edit Argon2 hashes.

**Can I run more than one Nimbo container against the same volume?**
No. SQLite and the file store assume a single writer process. Multiple replicas
or shared network filesystems are unsupported.

**Is my data encrypted at rest?**
Only provider API keys are encrypted by Nimbo, using `secret.key`. Notes,
database rows, uploaded blobs and backups are plaintext application data. Use
host/filesystem encryption (for example LUKS) and encrypted backup storage if
disk theft or backup exposure is in scope.

**Can I use Nimbo over plain HTTP on my LAN?**
Only as a deliberate, temporary risk. Plain HTTP exposes passwords, session
cookies and device tokens to anyone on the path. The Android app requires an
explicit insecure-allow setting for HTTP and warns that the token has
account-level access. Prefer TLS or an SSH tunnel.

**Which token should the Android app use?**
Create a **backup-scoped** device token. It is bound to the `Phone Backups`
folder subtree and cannot use docs, agent, settings, shares or token
administration. A full-scope token grants the same access as the account;
treat it like a password and revoke it when a device is retired.

**Does the Android app upload only on Wi-Fi?**
Its work constraint is **unmetered**, which is not the same as "Wi-Fi only".
Metered Wi-Fi is excluded, and some unmetered non-Wi-Fi networks may qualify.
The app's Test & Save action is a direct request and does not use that
constraint.

**How do I move the instance to a new host?**
Stop the source, take an offline whole-volume backup, copy the archive and the
image version/digest, restore and verify on the target, then start the proxy.
Do not run both hosts against copies of the same data.

**Where are the security reporting instructions?**
See [`../SECURITY.md`](../SECURITY.md). Do not include secrets or production
data in a public issue.

## Related documentation

- [`architecture.md`](architecture.md) — components, data flow and known
  limitations
- [`../README.md`](../README.md) — quick start and configuration summary
- [`../SECURITY.md`](../SECURITY.md) — vulnerability reporting and hardening
- [`../CHANGELOG.md`](../CHANGELOG.md) — release history
