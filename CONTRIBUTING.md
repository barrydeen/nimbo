# Contributing to Nimbo

Thanks for taking the time to contribute. Nimbo is a small, self-hosted project,
and focused bug reports, documentation fixes and well-scoped pull requests are
all welcome.

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

- Search existing [issues](https://github.com/barrydeen/nimbo/issues) before
  opening a new one.
- For security problems, **do not open a public issue**. Follow
  [SECURITY.md](SECURITY.md).
- For larger features, open an issue first so the approach can be discussed
  before you invest time in an implementation.

## Development setup

Requirements:

- Rust stable (the backend uses edition 2021)
- Node.js 22 or newer
- SQLite is embedded; no external database is required

```bash
# terminal 1 - backend
cd backend
NIMBO_DATA_DIR=../data cargo run

# terminal 2 - frontend (Vite proxies /api to :8080)
cd frontend
npm install
npm run dev
```

Open the Vite URL (normally <http://localhost:5173>) and complete the first-run
setup wizard.

To run the production-style single binary instead, build the frontend first and
point the backend at it:

```bash
cd frontend && npm ci && npm run build
cd ../backend
NIMBO_DATA_DIR=../data NIMBO_STATIC_DIR=../frontend/build cargo run
```

## Checks

Please run the same checks as CI before opening a pull request:

```bash
cd backend
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked

cd ../frontend
npm ci
npm run check
npm run build
node scripts/check-sanitizer.mjs

cd ..
mkdir -p /tmp/opencode
python3 scripts/verify-security-regressions.py --binary backend/target/release/nimbo
docker compose -f docker/docker-compose.yml config --quiet
python3 scripts/verify-operations.py
docker build -f docker/Dockerfile -t nimbo:local .
```

The release binary plus `scripts/verify-security-regressions.py` starts a
disposable instance and exercises setup-token enforcement, login bounding,
escaped snippets, symlink rejection, serialized saves/renames, tree-cycle
prevention, backup-token scope/replacement, 416 ranges, same-origin PDF
framing, cross-origin mutation rejection and public payload minimization. CI
fails if any regression check fails.

The Docker CI job also starts a disposable container, exercises first-run setup,
creates a note, restarts the container on the same named volume and verifies the
note survives. Run that smoke test when changing `docker/**`, persistence or
startup behavior. The production Compose example can be syntax-checked with:

```bash
NIMBO_IMAGE=nimbo:local NIMBO_DOMAIN=example.com \
  NIMBO_SETUP_TOKEN="$(openssl rand -hex 32)" \
  docker compose -f docker/docker-compose.production.yml config --quiet
```

The Android companion app is a separate Gradle project:

```bash
cd android
./gradlew :app:testDebugUnitTest :app:lintDebug :app:assembleDebug --no-daemon
```

`scripts/verify-operations.py` checks deployment defaults, required
documentation claims, CI job coverage and obvious committed secret/private
files. It must exit zero before a deployment or documentation change is merged.

## CI and release jobs

`.github/workflows/ci.yml` runs backend format/Clippy/tests, frontend
install/check/build/sanitizer controls, deterministic dependency audits, Compose
configuration checks, a Docker build and a persistence/health smoke test, and a
disposable release-binary run of the integrated security regression suite. All
pull-request jobs use read-only repository permissions.

`.github/workflows/android.yml` validates the Gradle wrapper, runs Android unit
tests and lint, and builds the debug APK.

`.github/workflows/release.yml` publishes versioned images to GHCR only on
`v*.*.*` tags, with provenance, SBOM generation and OCI source/revision labels.
`.github/workflows/android-release.yml` refuses to produce a release APK unless
all Android signing secrets are present. The expected secret names are listed in
[`SECURITY.md`](SECURITY.md).

Keep workflow actions pinned to reviewed full commit SHAs with the version in a
trailing comment; `scripts/verify-operations.py` rejects floating refs. Docker
base images and the proxy default are digest-pinned and remain overridable only
for reviewed experiments. The Gradle wrapper pins and validates its distribution
checksum.

Do not add debug-signed APKs to a release. If the signing secrets are not
configured, the workflow intentionally produces no release artifact; document
that state in the release notes instead of weakening the signing check. Before
tagging, bump the Android `versionCode` and set `versionName` to the tag without
the `v` prefix (for example tag `v0.2.0`, `versionName = "0.2.0"`); the release
workflow verifies that match and retains signing identity across builds.

## Documentation claims

Versioned documentation is part of the product:

- Keep deployment defaults and documented limits in sync with the code.
- Distinguish provider-key encryption from note/blob encryption; do not claim
  application-level encryption at rest for data that is plaintext.
- Do not claim a killable parser wall-clock budget, online backup, multi-replica
  support or per-user file permissions unless the code enforces it.
- Update `docs/operations.md` when backup, restore, upgrade or retention
  behavior changes.
- Add a `CHANGELOG.md` entry for user-visible security, deployment or recovery
  changes.

## Code style

- Rust: `rustfmt` defaults; keep `cargo clippy -D warnings` clean.
- TypeScript/Svelte: follow the existing tabs, naming and component structure.
- Keep the vault and file-store invariants intact: documents stay plain `.md`
  files, uploaded blobs stay content-addressed, and paths are validated before
  touching disk.
- Add tests for behaviour changes where practical. Backend tests live next to
  the code they cover.
- Avoid new runtime services. Nimbo is intentionally a single binary with an
  embedded SQLite database.

## Pull requests

- Keep each pull request focused on one change.
- Describe the problem, the approach and any trade-offs.
- Include the commands you ran and their results.
- Update `README.md`, `docs/architecture.md` or the API list when behaviour
  changes.
- Link the issue being fixed, for example `Fixes #123`.
- Expect review comments; maintainers may ask for a smaller scope or extra
  tests.

## Commit messages

Use a short imperative subject line, optionally followed by a body:

```
files: reject empty folder names

The API previously accepted a folder with a blank name. Validate the
trimmed name and return 400 before inserting the row.
```

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
