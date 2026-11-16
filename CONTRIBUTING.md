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
cargo clippy -- -D warnings
cargo test

cd ../frontend
npm ci
npm run check
npm run build
```

The Android companion app is a separate Gradle project:

```bash
cd android
./gradlew :app:assembleDebug
```

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
